//! Synced lyrics from LRCLIB (https://lrclib.net). Only the title and artist
//! leave the machine; the duration is used locally to pick the right match.
//! Requests run on one service thread, and results are cached in memory.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

const API: &str = "https://lrclib.net/api/search";
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
}

#[derive(Clone, Debug)]
pub struct Line {
    pub at: Duration,
    pub text: String,
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
            while let Ok(req) = rx.recv_blocking() {
                let lines = match cache.get(&req.key) {
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
}
