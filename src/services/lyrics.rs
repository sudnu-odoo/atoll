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
const TIMEOUT: Duration = Duration::from_secs(8);
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
    /// `None` when the song has no synced lyrics (or the lookup failed).
    pub lines: Option<Vec<Line>>,
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
            let mut cache: HashMap<Key, Option<Vec<Line>>> = HashMap::new();
            let mut translated: HashMap<Key, Option<Vec<Line>>> = HashMap::new();
            while let Ok(req) = rx.recv_blocking() {
                let mut lines = match cache.get(&req.key) {
                    Some(hit) => hit.clone(),
                    None => {
                        let found = lookup(&client, &req).unwrap_or_else(|e| {
                            tracing::debug!("lyrics lookup failed: {e}");
                            None
                        });
                        cache.insert(req.key.clone(), found.clone());
                        found
                    }
                };
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
}

fn lookup(
    client: &reqwest::blocking::Client,
    req: &Request,
) -> Result<Option<Vec<Line>>, Box<dyn std::error::Error>> {
    let body = client
        .get(API)
        .query(&[
            ("track_name", req.key.title.as_str()),
            ("artist_name", req.key.artist.as_str()),
        ])
        .send()?
        .error_for_status()?
        .bytes()?;
    let hits: Vec<Hit> = serde_json::from_slice(&body)?;

    let close = |h: &Hit| match (req.length, h.duration) {
        (Some(len), Some(d)) => (len.as_secs_f64() - d).abs() <= DURATION_SLACK.as_secs_f64(),
        _ => true,
    };
    let best = hits
        .iter()
        .filter(|h| h.synced_lyrics.is_some())
        .find(|h| close(h))
        .and_then(|h| h.synced_lyrics.as_deref())
        .map(parse_lrc)
        .filter(|lines| !lines.is_empty());
    Ok(best)
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
    fn parses_translate_reply() {
        let reply = br#"[["Hello\nWorld","ja"]]"#;
        let (text, source) = parse_translation(reply).unwrap();
        assert_eq!(text, "Hello\nWorld");
        assert_eq!(source.as_deref(), Some("ja"));
        assert!(parse_translation(b"{}").is_none());
    }
}
