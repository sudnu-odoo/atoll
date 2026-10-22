//! Media players over MPRIS. Runs on its own thread with its own executor
//! and talks to the UI only through channels, so a slow or misbehaving player
//! can never stall the island.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::stream::{LocalBoxStream, SelectAll};
use futures_util::StreamExt;
use zbus::fdo::DBusProxy;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{proxy, Connection};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld re-exports whichever player is active, so listening to it
/// would show every track twice.
const IGNORED: &[&str] = &["org.mpris.MediaPlayer2.playerctld"];
const ART_TIMEOUT: Duration = Duration::from_secs(5);
const ART_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait Player {
    fn play_pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn set_position(&self, track_id: &ObjectPath<'_>, position: i64) -> zbus::Result<()>;
    fn seek(&self, offset: i64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    // MPRIS never signals Position changes, so it must not be cached.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> zbus::Result<i64>;
    #[zbus(signal)]
    fn seeked(&self, position: i64) -> zbus::Result<()>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    /// Short player name for display, e.g. "Spotify".
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    /// MPRIS SetPosition needs it, so a seek can't land on the wrong track.
    pub track_id: Option<OwnedObjectPath>,
    pub length: Option<Duration>,
    pub playing: bool,
    /// Position when the snapshot was taken; the UI extrapolates from `at`.
    pub position: Duration,
    pub at: Instant,
}

impl NowPlaying {
    pub fn position_now(&self) -> Duration {
        let pos = if self.playing {
            self.position + self.at.elapsed()
        } else {
            self.position
        };
        self.length.map_or(pos, |len| pos.min(len))
    }
}

#[derive(Clone)]
pub enum Update {
    /// `None` means nothing is playing (or no player exists).
    Track(Option<NowPlaying>),
    Art {
        url: String,
        bytes: Vec<u8>,
    },
}

pub enum Command {
    PlayPause,
    Next,
    Previous,
    /// Jump to an absolute position in the current track.
    SetPosition(Duration),
}

/// Starts the service thread. If the session bus is unreachable the thread
/// reports "nothing playing" once and exits, which hides the music card.
pub fn spawn(updates: async_channel::Sender<Update>) -> async_channel::Sender<Command> {
    let (cmd_tx, cmd_rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("mpris".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run(&updates, cmd_rx)) {
                tracing::warn!("MPRIS service stopped: {e}");
                let _ = updates.send_blocking(Update::Track(None));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start MPRIS thread: {e}");
    }
    cmd_tx
}

enum Event {
    Owner { name: String, present: bool },
    Changed(String),
    Cmd(Command),
}

async fn run(
    updates: &async_channel::Sender<Update>,
    commands: async_channel::Receiver<Command>,
) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    let dbus = DBusProxy::new(&conn).await?;

    let mut events: SelectAll<LocalBoxStream<'static, Event>> = SelectAll::new();
    events.push(commands.map(Event::Cmd).boxed_local());
    events.push(
        dbus.receive_name_owner_changed()
            .await?
            .filter_map(|sig| async move {
                let args = sig.args().ok()?;
                let name = args.name().to_string();
                is_player(&name).then(|| Event::Owner {
                    present: args.new_owner().is_some(),
                    name,
                })
            })
            .boxed_local(),
    );

    let mut players: HashMap<String, PlayerProxy<'static>> = HashMap::new();
    for name in dbus.list_names().await? {
        let name = name.to_string();
        if is_player(&name) {
            add_player(&conn, &name, &mut players, &mut events).await;
        }
    }

    let mut active: Option<String> = None;
    let mut last_sent: Option<NowPlaying> = None;
    let mut last_art: Option<String> = None;
    publish(
        &players,
        &mut active,
        &mut last_sent,
        &mut last_art,
        updates,
    )
    .await;

    while let Some(event) = events.next().await {
        match event {
            Event::Owner {
                name,
                present: true,
            } => {
                add_player(&conn, &name, &mut players, &mut events).await;
            }
            Event::Owner {
                name,
                present: false,
            } => {
                tracing::debug!("player gone: {name}");
                players.remove(&name);
                if active.as_ref() == Some(&name) {
                    active = None;
                }
            }
            Event::Changed(name) => {
                // A player that starts playing takes over the island.
                if let Some(p) = players.get(&name) {
                    if p.playback_status().await.is_ok_and(|s| s == "Playing") {
                        active = Some(name);
                    }
                }
            }
            Event::Cmd(cmd) => {
                let seeking = matches!(cmd, Command::SetPosition(_));
                if let Some(p) = active.as_ref().and_then(|n| players.get(n)) {
                    let res = match cmd {
                        Command::PlayPause => p.play_pause().await,
                        Command::Next => p.next().await,
                        Command::Previous => p.previous().await,
                        Command::SetPosition(to) => seek_to(p, &last_sent, to).await,
                    };
                    if let Err(e) = res {
                        tracing::warn!("player command failed: {e}");
                    }
                }
                if !seeking {
                    continue; // the player will signal its new state itself
                }
                // Not every player emits Seeked, and small seeks would fall
                // under the "unchanged" threshold: force a fresh snapshot.
                last_sent = None;
            }
        }
        publish(
            &players,
            &mut active,
            &mut last_sent,
            &mut last_art,
            updates,
        )
        .await;
    }
    Ok(())
}

/// Prefers SetPosition (absolute, tied to the track); falls back to a
/// relative Seek for players that don't expose a track id.
async fn seek_to(
    p: &PlayerProxy<'static>,
    current: &Option<NowPlaying>,
    to: Duration,
) -> zbus::Result<()> {
    let to_us = i64::try_from(to.as_micros()).unwrap_or(i64::MAX);
    if let Some(id) = current.as_ref().and_then(|np| np.track_id.as_ref()) {
        return p.set_position(id, to_us).await;
    }
    let now_us = p.position().await?;
    p.seek(to_us - now_us).await
}

fn is_player(name: &str) -> bool {
    name.starts_with(PREFIX) && !IGNORED.contains(&name)
}

async fn add_player(
    conn: &Connection,
    name: &str,
    players: &mut HashMap<String, PlayerProxy<'static>>,
    events: &mut SelectAll<LocalBoxStream<'static, Event>>,
) {
    let proxy = match PlayerProxy::builder(conn).destination(name.to_owned()) {
        Ok(b) => b.build().await,
        Err(e) => Err(e),
    };
    let proxy = match proxy {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("cannot watch player {name}: {e}");
            return;
        }
    };
    tracing::debug!("player found: {name}");

    events.push(changed(proxy.receive_playback_status_changed().await, name));
    events.push(changed(proxy.receive_metadata_changed().await, name));
    if let Ok(seeked) = proxy.receive_seeked().await {
        events.push(changed(seeked, name));
    }
    players.insert(name.to_owned(), proxy);
}

/// Any signal from a player just means "re-read it"; the payloads differ
/// per stream but are never needed.
fn changed<S>(stream: S, name: &str) -> LocalBoxStream<'static, Event>
where
    S: futures_util::Stream + 'static,
{
    let name = name.to_owned();
    stream
        .map(move |_| Event::Changed(name.clone()))
        .boxed_local()
}

/// Picks the player to show and sends its state if anything changed.
async fn publish(
    players: &HashMap<String, PlayerProxy<'static>>,
    active: &mut Option<String>,
    last_sent: &mut Option<NowPlaying>,
    last_art: &mut Option<String>,
    updates: &async_channel::Sender<Update>,
) {
    if active.as_ref().is_none_or(|n| !players.contains_key(n)) {
        *active = pick_active(players).await;
    }
    let now = match active.as_ref().and_then(|n| players.get(n).map(|p| (n, p))) {
        Some((name, p)) => snapshot(name, p).await,
        None => None,
    };

    // Compare without the timestamp, which is always new.
    let same = match (&now, &*last_sent) {
        (Some(a), Some(b)) => {
            a.title == b.title
                && a.artist == b.artist
                && a.playing == b.playing
                && a.art_url == b.art_url
                && a.length == b.length
                && a.position_now().abs_diff(b.position_now()) < Duration::from_secs(2)
        }
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }

    let art = now.as_ref().and_then(|n| n.art_url.clone());
    if art != *last_art {
        if let Some(url) = &art {
            fetch_art(url.clone(), updates.clone());
        }
        last_art.clone_from(&art);
    }
    last_sent.clone_from(&now);
    let _ = updates.send(Update::Track(now)).await;
}

async fn pick_active(players: &HashMap<String, PlayerProxy<'static>>) -> Option<String> {
    let mut fallback = None;
    for (name, p) in players {
        match p.playback_status().await.as_deref() {
            Ok("Playing") => return Some(name.clone()),
            Ok("Paused") => fallback = Some(name.clone()),
            _ => {}
        }
    }
    fallback
}

async fn snapshot(name: &str, p: &PlayerProxy<'static>) -> Option<NowPlaying> {
    let status = p.playback_status().await.ok()?;
    let meta = p.metadata().await.unwrap_or_default();
    let title = text(&meta, "xesam:title").unwrap_or_default();
    // Browsers keep an MPRIS entry for idle tabs; without a title there's
    // nothing worth showing.
    if title.is_empty() || status == "Stopped" {
        return None;
    }
    let position = p.position().await.unwrap_or(0);

    Some(NowPlaying {
        player: display_name(name),
        title,
        artist: artists(&meta).unwrap_or_default(),
        album: text(&meta, "xesam:album").unwrap_or_default(),
        art_url: text(&meta, "mpris:artUrl").filter(|u| !u.is_empty()),
        track_id: meta
            .get("mpris:trackid")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| OwnedObjectPath::try_from(v).ok()),
        length: micros(&meta, "mpris:length").filter(|d| !d.is_zero()),
        playing: status == "Playing",
        position: Duration::from_micros(position.max(0).unsigned_abs()),
        at: Instant::now(),
    })
}

fn text(meta: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    String::try_from(meta.get(key)?.try_clone().ok()?).ok()
}

fn artists(meta: &HashMap<String, OwnedValue>) -> Option<String> {
    let list = Vec::<String>::try_from(meta.get("xesam:artist")?.try_clone().ok()?).ok()?;
    Some(list.join(", "))
}

/// `mpris:length` is specified as i64 but some players send u64.
fn micros(meta: &HashMap<String, OwnedValue>, key: &str) -> Option<Duration> {
    let v = meta.get(key)?;
    let us = i64::try_from(v.try_clone().ok()?)
        .ok()
        .map(|n| n.max(0).unsigned_abs())
        .or_else(|| u64::try_from(v.try_clone().ok()?).ok())?;
    Some(Duration::from_micros(us))
}

/// "org.mpris.MediaPlayer2.spotify" or "...brave.instance3573" -> "Spotify"/"Brave".
fn display_name(bus_name: &str) -> String {
    let short = bus_name
        .trim_start_matches(PREFIX)
        .split('.')
        .next()
        .unwrap_or_default();
    let mut chars = short.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Loads cover art off the service loop: local files are read directly,
/// remote URLs (Spotify) are downloaded on a short-lived thread.
fn fetch_art(url: String, updates: async_channel::Sender<Update>) {
    let spawned = std::thread::Builder::new()
        .name("art".into())
        .spawn(move || {
            let bytes = if let Some(path) = url.strip_prefix("file://") {
                std::fs::read(path).map_err(|e| e.to_string())
            } else if url.starts_with("https://") || url.starts_with("http://") {
                download(&url)
            } else {
                Err("unsupported scheme".into())
            };
            match bytes {
                Ok(bytes) => {
                    let _ = updates.send_blocking(Update::Art { url, bytes });
                }
                Err(e) => tracing::debug!("no cover art for {url}: {e}"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start art thread: {e}");
    }
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let client = reqwest::blocking::Client::builder()
        .timeout(ART_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    resp.take(ART_MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}
