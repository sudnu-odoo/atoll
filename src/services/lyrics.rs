//! Synced lyrics from LRCLIB (https://lrclib.net). Only the title and artist
//! leave the machine; the duration is used locally to pick the right match.
//! Optionally (off by default, the user's choice in Settings) the lyric text
//! is sent to Google Translate for English lines under the originals.
//! Requests run on one service thread, and results are cached in memory.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

const API: &str = "https://lrclib.net/api/search";
/// Google's keyless endpoint used by its Chrome extension (the better-known
/// `translate_a/single` answers many networks with a bot check).
const TRANSLATE_API: &str = "https://clients5.google.com/translate_a/t";
/// Lines per translate request, keeping the URL well under length limits.
const TRANSLATE_CHUNK: usize = 25;
/// LRCLIB is sometimes slow to answer; better late than "not found".
const TIMEOUT: Duration = Duration::from_secs(15);
/// Same song, different releases: lengths differ by a few seconds at most.
const DURATION_SLACK: Duration = Duration::from_secs(3);
const USER_AGENT: &str = concat!("Atoll/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub title: String,
    pub artist: String,
}

pub struct Request {
    pub key: Key,
    pub length: Option<Duration>,
    /// Add English translations (when the song isn't English already).
    pub translate: bool,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub at: Duration,
    pub text: String,
    pub translation: Option<String>,
}

#[derive(Clone)]
pub struct Update {
    pub key: Key,
    /// `None` when the song has no lyrics, or LRCLIB couldn't be reached.
    pub lines: Option<Vec<Line>>,
    /// The lookup failed (offline, timeout): "none" isn't known, and the
    /// next request tries again.
    pub failed: bool,
    /// False for plain lyrics, whose timing is only an even estimate.
    pub synced: bool,
}

#[derive(Clone)]
struct Found {
    lines: Vec<Line>,
    synced: bool,
}

pub fn spawn(updates: async_channel::Sender<Update>) -> async_channel::Sender<Request> {
    let (tx, rx) = async_channel::unbounded::<Request>();
    let spawned = std::thread::Builder::new()
        .name("lyrics".into())
        .spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(TIMEOUT)
                .user_agent(USER_AGENT)
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("lyrics disabled, no HTTP client: {e}");
                    return;
                }
            };
            let mut cache: HashMap<Key, Option<Found>> = HashMap::new();
            let mut translated: HashMap<Key, Option<Vec<Line>>> = HashMap::new();
            while let Ok(req) = rx.recv_blocking() {
                let mut failed = false;
                let found = match cache.get(&req.key) {
                    Some(hit) => hit.clone(),
                    // Only answers are cached: a failure (e.g. right after
                    // waking from sleep) must not mean "no lyrics" for good.
                    None => match lookup(&client, &req) {
                        Ok(found) => {
                            cache.insert(req.key.clone(), found.clone());
                            found
                        }
                        Err(e) => {
                            tracing::warn!("lyrics lookup failed: {e}");
                            failed = true;
                            None
                        }
                    },
                };
                let synced = found.as_ref().is_some_and(|f| f.synced);
                let mut lines = found.map(|f| f.lines);
                if req.translate {
                    if let Some(original) = &lines {
                        let done = translated.entry(req.key.clone()).or_insert_with(|| {
                            translate(&client, original).unwrap_or_else(|e| {
                                tracing::debug!("lyrics translation failed: {e}");
                                None
                            })
                        });
                        if done.is_some() {
                            lines = done.clone();
                        }
                    }
                }
                if updates
                    .send_blocking(Update {
                        key: req.key,
                        lines,
                        failed,
                        synced,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start lyrics thread: {e}");
    }
    tx
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
    duration: Option<f64>,
    synced_lyrics: Option<String>,
    plain_lyrics: Option<String>,
}

/// Synced lyrics of the right length when there are any; otherwise plain
/// lyrics, paced evenly over the song. Tries the title as the player gives
/// it, then cleaned of release notes ("(Live)", "- Remastered 2011", ...),
/// which LRCLIB entries usually leave out.
fn lookup(
    client: &reqwest::blocking::Client,
    req: &Request,
) -> Result<Option<Found>, Box<dyn std::error::Error>> {
    let mut attempts = vec![(req.key.title.clone(), req.key.artist.clone())];
    let cleaned = (clean_title(&req.key.title), main_artist(&req.key.artist));
    if cleaned != attempts[0] && !cleaned.0.is_empty() {
        attempts.push(cleaned);
    }
    let close = |h: &Hit| match (req.length, h.duration) {
        (Some(len), Some(d)) => (len.as_secs_f64() - d).abs() <= DURATION_SLACK.as_secs_f64(),
        _ => true,
    };
    let mut plain: Option<String> = None;
    for (title, artist) in &attempts {
        let body = client
            .get(API)
            .query(&[
                ("track_name", title.as_str()),
                ("artist_name", artist.as_str()),
            ])
            .send()?
            .error_for_status()?
            .bytes()?;
        // LRCLIB answers some odd titles with an error object, not a list.
        let hits: Vec<Hit> = serde_json::from_slice(&body).unwrap_or_default();
        let synced = hits
            .iter()
            .filter(|h| close(h))
            .filter_map(|h| h.synced_lyrics.as_deref())
            .map(parse_lrc)
            .find(|lines| !lines.is_empty());
        if let Some(lines) = synced {
            return Ok(Some(Found {
                lines,
                synced: true,
            }));
        }
        // A synced text of another length (e.g. the studio version of a live
        // recording) would run out of step; its words are still right.
        plain = plain.or_else(|| {
            hits.iter().find_map(|h| {
                h.plain_lyrics
                    .clone()
                    .filter(|p| !p.trim().is_empty())
                    .or_else(|| h.synced_lyrics.as_deref().map(strip_stamps))
            })
        });
    }
    Ok(plain.and_then(|p| {
        let lines = pace(&p, req.length?);
        (!lines.is_empty()).then_some(Found {
            lines,
            synced: false,
        })
    }))
}

/// "Song (Live at X)" / "Song - Remastered 2011" / "Song [feat. Y]" → "Song".
fn clean_title(title: &str) -> String {
    let mut t = title.to_owned();
    for (open, close) in [('(', ')'), ('[', ']')] {
        while let (Some(a), Some(b)) = (t.find(open), t.find(close)) {
            if b <= a {
                break;
            }
            t.replace_range(a..=b, "");
        }
    }
    if let Some(i) = t.find(" - ") {
        t.truncate(i);
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "A, B & C feat. D" → "A".
fn main_artist(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let cut = [",", " & ", " feat", " ft.", " x ", " with "]
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(artist.len());
    artist.get(..cut).unwrap_or(artist).trim().to_owned()
}

fn strip_stamps(lrc: &str) -> String {
    parse_lrc(lrc)
        .into_iter()
        .map(|l| l.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Plain lyrics spread evenly between a short intro and outro: close enough
/// to follow along, and labelled as not synced.
fn pace(text: &str, length: Duration) -> Vec<Line> {
    let rows: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let len = length.as_secs_f64();
    let (start, end) = (len * 0.08, len * 0.92);
    let step = (end - start) / rows.len() as f64;
    rows.iter()
        .enumerate()
        .map(|(i, text)| Line {
            at: Duration::from_secs_f64(start + step * i as f64),
            text: (*text).to_owned(),
            translation: None,
        })
        .collect()
}

/// The lines with English translations added, or `None` when the song is
/// already English (nothing to add).
fn translate(
    client: &reqwest::blocking::Client,
    lines: &[Line],
) -> Result<Option<Vec<Line>>, Box<dyn std::error::Error>> {
    let mut unique: Vec<&str> = Vec::new();
    for l in lines {
        if !l.text.is_empty() && !unique.contains(&l.text.as_str()) {
            unique.push(&l.text);
        }
    }
    let mut english: HashMap<&str, String> = HashMap::new();
    let mut foreign = false;
    for chunk in unique.chunks(TRANSLATE_CHUNK) {
        let q = chunk.join("\n");
        let body = client
            .get(TRANSLATE_API)
            .query(&[
                ("client", "dict-chrome-ex"),
                ("sl", "auto"),
                ("tl", "en"),
                ("q", q.as_str()),
            ])
            .send()?
            .error_for_status()?
            .bytes()?;
        let (text, source) = parse_translation(&body).ok_or("unexpected translate reply")?;
        foreign |= source.as_deref() != Some("en");
        let out: Vec<&str> = text.split('\n').collect();
        // Lines must pair up one to one, or they'd sit under the wrong line.
        if out.len() != chunk.len() {
            return Err("translation lines don't match".into());
        }
        for (orig, en) in chunk.iter().zip(out) {
            let en = en.trim();
            if !en.is_empty() && !en.eq_ignore_ascii_case(orig.trim()) {
                english.insert(orig, en.to_owned());
            }
        }
    }
    if !foreign || english.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        lines
            .iter()
            .map(|l| Line {
                translation: english.get(l.text.as_str()).cloned(),
                ..l.clone()
            })
            .collect(),
    ))
}

/// The reply is `[["translated", "ja"]]`: the text and the detected source
/// language (`["translated"]` when there's no detection).
fn parse_translation(body: &[u8]) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let first = v.get(0)?;
    if let Some(text) = first.as_str() {
        return Some((text.to_owned(), None));
    }
    let text = first.get(0)?.as_str()?.to_owned();
    let source = first.get(1).and_then(|s| s.as_str()).map(str::to_owned);
    Some((text, source))
}

/// Parses "[mm:ss.xx] text" lines. A line may carry several timestamps.
fn parse_lrc(lrc: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { break };
            if let Some(at) = parse_stamp(&inner[..end]) {
                stamps.push(at);
            }
            rest = &inner[end + 1..];
        }
        let text = rest.trim();
        for at in stamps {
            lines.push(Line {
                at,
                text: text.to_owned(),
                translation: None,
            });
        }
    }
    lines.sort_by_key(|l| l.at);
    lines
}

fn parse_stamp(s: &str) -> Option<Duration> {
    let (min, sec) = s.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec: f64 = sec.trim().parse().ok()?;
    Some(Duration::from_secs(min * 60) + Duration::from_secs_f64(sec.max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let lines = parse_lrc("[ar: x]\n[00:01.50] a\n[00:03.00][00:05.25] b\n[00:02.00]");
        let got: Vec<(u128, &str)> = lines
            .iter()
            .map(|l| (l.at.as_millis(), l.text.as_str()))
            .collect();
        assert_eq!(got, vec![(1500, "a"), (2000, ""), (3000, "b"), (5250, "b")]);
    }

    #[test]
    fn cleans_titles_and_artists() {
        assert_eq!(
            clean_title("Hold Me While You Wait (Recorded at Spotify Studios NYC)"),
            "Hold Me While You Wait"
        );
        assert_eq!(clean_title("Let It Be - Remastered 2009"), "Let It Be");
        assert_eq!(clean_title("Song [feat. X] (Live)"), "Song");
        assert_eq!(main_artist("Lewis Capaldi"), "Lewis Capaldi");
        assert_eq!(main_artist("A, B & C"), "A");
        assert_eq!(main_artist("Calvin Harris feat. Rihanna"), "Calvin Harris");
    }

    #[test]
    fn plain_lyrics_are_paced_over_the_song() {
        let lines = pace("one\n\ntwo\nthree\n", Duration::from_secs(100));
        let at: Vec<u64> = lines.iter().map(|l| l.at.as_secs()).collect();
        assert_eq!(at, vec![8, 36, 64]);
        assert!(pace("", Duration::from_secs(100)).is_empty());
    }

    #[test]
    fn parses_translate_reply() {
        let reply = br#"[["Hello\nWorld","ja"]]"#;
        let (text, source) = parse_translation(reply).unwrap();
        assert_eq!(text, "Hello\nWorld");
        assert_eq!(source.as_deref(), Some("ja"));
        assert!(parse_translation(b"{}").is_none());
    }
}
