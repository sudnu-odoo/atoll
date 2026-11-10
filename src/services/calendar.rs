//! Upcoming meetings from GNOME's calendar service (Evolution Data Server),
//! which GNOME Online Accounts keeps in sync with Google Calendar. Atoll
//! never sees account credentials; it only reads the local service.
//!
//! Refreshes when a calendar's revision changes (EDS synced something) and
//! every half hour (to roll the time window); idle otherwise.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use futures_util::stream::{LocalBoxStream, SelectAll};
use futures_util::StreamExt;
use zbus::fdo::{ObjectManagerProxy, PropertiesProxy};
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const SOURCES_BUS: &str = "org.gnome.evolution.dataserver.Sources5";
const SOURCES_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
const CALENDAR_BUS: &str = "org.gnome.evolution.dataserver.Calendar8";
const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.CalendarFactory";
const CALENDAR_IFACE: &str = "org.gnome.evolution.dataserver.Calendar";
/// How far ahead (and behind, for meetings in progress) to look.
const LOOK_BEHIND: chrono::Duration = chrono::Duration::hours(12);
const LOOK_AHEAD: chrono::Duration = chrono::Duration::hours(36);
const ROLL_WINDOW: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Meeting {
    /// Unique per occurrence (event UID + start), to remind only once.
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub start: i64,
    pub end: i64,
    /// Meet, Zoom or Teams link, if the event has one.
    pub join_url: Option<String>,
    /// Holidays, birthdays, "out of office": shown in the month view, never
    /// reminded about.
    pub all_day: bool,
}

pub enum Update {
    /// Timed meetings from a little before now to a day and a half ahead.
    Upcoming(Vec<Meeting>),
    /// Everything in one month (local time), for the month view.
    Month {
        year: i32,
        month: u32,
        events: Vec<Meeting>,
    },
}

/// Starts the service; send `(year, month)` on the returned channel to get
/// that month's events back as `Update::Month`.
pub fn spawn(updates: async_channel::Sender<Update>) -> async_channel::Sender<(i32, u32)> {
    let (months_tx, months_rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("calendar".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run(&updates, months_rx)) {
                // No calendar service (or no accounts): the card stays empty.
                tracing::debug!("calendar disabled: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start calendar thread: {e}");
    }
    months_tx
}

struct Calendar<'a> {
    proxy: Proxy<'a>,
    /// The account's own address, to skip meetings it declined.
    owner: Option<String>,
}

enum Wake {
    Changed,
    Roll,
    Month(i32, u32),
}

async fn run(
    updates: &async_channel::Sender<Update>,
    months: async_channel::Receiver<(i32, u32)>,
) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    let mut wakes: SelectAll<LocalBoxStream<'static, Wake>> = SelectAll::new();
    let mut calendars = Vec::new();
    for uid in calendar_sources(&conn).await? {
        match open(&conn, &uid).await {
            Ok((cal, changes)) => {
                wakes.push(changes);
                calendars.push(cal);
            }
            Err(e) => tracing::debug!("cannot open calendar {uid}: {e}"),
        }
    }
    tracing::debug!("reading {} calendars", calendars.len());
    wakes.push(
        async_io::Timer::interval(ROLL_WINDOW)
            .map(|_| Wake::Roll)
            .boxed_local(),
    );
    wakes.push(months.map(|(y, m)| Wake::Month(y, m)).boxed_local());

    // Months someone is looking at; refreshed too when the calendar changes.
    let mut shown: Vec<(i32, u32)> = Vec::new();
    let mut refresh_upcoming = true;
    loop {
        if refresh_upcoming {
            let now = Utc::now();
            let meetings: Vec<Meeting> = fetch(&calendars, now - LOOK_BEHIND, now + LOOK_AHEAD)
                .await
                .into_iter()
                .filter(|m| !m.all_day)
                .collect();
            tracing::debug!("{} upcoming meetings", meetings.len());
            if updates.send(Update::Upcoming(meetings)).await.is_err() {
                return Ok(());
            }
        }
        // One sync sends several revision bumps; wait for any, then refetch.
        match wakes.next().await {
            None => return Ok(()),
            Some(Wake::Roll) => refresh_upcoming = true,
            Some(Wake::Changed) => {
                refresh_upcoming = true;
                for &(y, m) in &shown {
                    if !send_month(&calendars, updates, y, m).await {
                        return Ok(());
                    }
                }
            }
            Some(Wake::Month(y, m)) => {
                refresh_upcoming = false;
                shown.retain(|s| *s != (y, m));
                shown.push((y, m));
                // Only a few months stay "live".
                if shown.len() > 3 {
                    shown.remove(0);
                }
                if !send_month(&calendars, updates, y, m).await {
                    return Ok(());
                }
            }
        }
    }
}

async fn send_month(
    calendars: &[Calendar<'_>],
    updates: &async_channel::Sender<Update>,
    year: i32,
    month: u32,
) -> bool {
    let Some((from, to)) = month_range(year, month) else {
        return true;
    };
    let events = fetch(calendars, from, to).await;
    updates
        .send(Update::Month {
            year,
            month,
            events,
        })
        .await
        .is_ok()
}

/// First instant of the month to first instant of the next, local time.
fn month_range(year: i32, month: u32) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    let at = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    };
    Some((at(first)?, at(next)?))
}

/// UIDs of enabled calendar sources (Google, CalDAV, local...).
async fn calendar_sources(conn: &Connection) -> zbus::Result<Vec<String>> {
    let manager = ObjectManagerProxy::builder(conn)
        .destination(SOURCES_BUS)?
        .path(SOURCES_PATH)?
        .build()
        .await?;
    let mut uids = Vec::new();
    for (_, ifaces) in manager.get_managed_objects().await? {
        let Some(props) = ifaces.get("org.gnome.evolution.dataserver.Source") else {
            continue;
        };
        let text = |key: &str| {
            props
                .get(key)
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| String::try_from(v).ok())
        };
        let (Some(uid), Some(data)) = (text("UID"), text("Data")) else {
            continue;
        };
        if data.contains("[Calendar]") && !data.contains("Enabled=false") {
            uids.push(uid);
        }
    }
    Ok(uids)
}

async fn open(
    conn: &Connection,
    uid: &str,
) -> zbus::Result<(Calendar<'static>, LocalBoxStream<'static, Wake>)> {
    let factory = Proxy::new(conn, CALENDAR_BUS, FACTORY_PATH, FACTORY_IFACE).await?;
    let (path, bus): (String, String) = factory.call("OpenCalendar", &(uid,)).await?;
    let path = OwnedObjectPath::try_from(path)?;
    let proxy = Proxy::new(conn, bus.clone(), path.clone(), CALENDAR_IFACE).await?;
    // Open returns the backend's properties as alternating key, value.
    let props: Vec<String> = proxy.call("Open", &()).await?;
    let owner = props
        .chunks(2)
        .find(|kv| kv.first().map(String::as_str) == Some("cal-email-address"))
        .and_then(|kv| kv.get(1))
        .map(|v| v.trim_matches('\'').to_lowercase());

    let changes = PropertiesProxy::builder(conn)
        .destination(bus)?
        .path(path)?
        .build()
        .await?
        .receive_properties_changed()
        .await?
        .map(|_| Wake::Changed)
        .boxed_local();
    Ok((Calendar { proxy, owner }, changes))
}

async fn fetch(calendars: &[Calendar<'_>], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Meeting> {
    let query = format!(
        "(occur-in-time-range? (make-time \"{}\") (make-time \"{}\"))",
        from.format("%Y%m%dT%H%M%SZ"),
        to.format("%Y%m%dT%H%M%SZ"),
    );
    let mut meetings = Vec::new();
    for cal in calendars {
        match cal
            .proxy
            .call::<_, _, Vec<String>>("GetObjectList", &(&query,))
            .await
        {
            Ok(objects) => meetings.extend(expand(&objects, from, to, cal.owner.as_deref())),
            Err(e) => tracing::debug!("calendar read failed: {e}"),
        }
    }
    meetings.sort_by_key(|m| m.start);
    meetings.dedup_by(|a, b| a.id == b.id);
    meetings
}

/// One parsed VEVENT.
#[derive(Default)]
struct Event {
    uid: String,
    title: String,
    start: Option<(String, Option<String>)>,
    end: Option<(String, Option<String>)>,
    duration: Option<String>,
    all_day: bool,
    rrule: Option<String>,
    exdates: Vec<(String, Option<String>)>,
    recurrence_id: Option<(String, Option<String>)>,
    cancelled: bool,
    declined_by: Vec<String>,
    join_url: Option<String>,
}

/// Turns EDS objects into meeting occurrences inside [from, to]. Recurring
/// events are expanded; per-occurrence edits replace their occurrence.
fn expand(
    objects: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    owner: Option<&str>,
) -> Vec<Meeting> {
    let events: Vec<Event> = objects.iter().filter_map(|o| parse_event(o)).collect();
    // Occurrences that were edited individually; the master skips them.
    let overridden: HashSet<(String, i64)> = events
        .iter()
        .filter_map(|e| {
            let (v, tz) = e.recurrence_id.as_ref()?;
            Some((e.uid.clone(), to_utc(v, tz.as_deref())?.timestamp()))
        })
        .collect();

    let mut out = Vec::new();
    for e in &events {
        if e.cancelled || owner.is_some_and(|o| e.declined_by.iter().any(|d| d == o)) {
            continue;
        }
        let Some((start_v, start_tz)) = &e.start else {
            continue;
        };
        // All-day dates become local midnights, so they expand and compare
        // like any other event.
        let start_v = &if e.all_day && start_v.len() == 8 {
            format!("{start_v}T000000")
        } else {
            start_v.clone()
        };
        let start_tz = &if e.all_day { None } else { start_tz.clone() };
        let Some(start) = to_utc(start_v, start_tz.as_deref()) else {
            continue;
        };
        let length = event_length(e, start);

        let starts: Vec<DateTime<Utc>> = match (&e.rrule, &e.recurrence_id) {
            (Some(rule), None) => occurrences(
                start_v,
                start_tz.as_deref(),
                rule,
                &e.exdates,
                from - length,
                to,
            )
            .unwrap_or_else(|| vec![start]),
            _ => vec![start],
        };
        for s in starts {
            if e.recurrence_id.is_none() && overridden.contains(&(e.uid.clone(), s.timestamp())) {
                continue;
            }
            let end = s + length;
            if end <= from || s >= to {
                continue;
            }
            out.push(Meeting {
                id: format!("{}@{}", e.uid, s.timestamp()),
                title: e.title.clone(),
                start: s.timestamp(),
                end: end.timestamp(),
                join_url: e.join_url.clone(),
                all_day: e.all_day,
            });
        }
    }
    out
}

fn event_length(e: &Event, start: DateTime<Utc>) -> chrono::Duration {
    let fallback = if e.all_day {
        chrono::Duration::days(1)
    } else {
        chrono::Duration::hours(1)
    };
    if let Some(end) = e
        .end
        .as_ref()
        .and_then(|(v, tz)| to_utc(v, tz.as_deref()))
        .filter(|end| *end > start)
    {
        return end - start;
    }
    e.duration
        .as_deref()
        .and_then(parse_duration)
        .unwrap_or(fallback)
}

fn occurrences(
    start_value: &str,
    tzid: Option<&str>,
    rule: &str,
    exdates: &[(String, Option<String>)],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<Vec<DateTime<Utc>>> {
    let dtstart = match tzid {
        Some(tz) => format!("DTSTART;TZID={tz}:{start_value}"),
        None => format!("DTSTART:{start_value}"),
    };
    let mut text = format!("{dtstart}\nRRULE:{rule}");
    for (v, tz) in exdates {
        match tz {
            Some(tz) => text.push_str(&format!("\nEXDATE;TZID={tz}:{v}")),
            None => text.push_str(&format!("\nEXDATE:{v}")),
        }
    }
    let set: rrule::RRuleSet = text.parse().ok()?;
    let utc = |d: DateTime<Utc>| d.with_timezone(&rrule::Tz::UTC);
    let result = set.after(utc(from)).before(utc(to)).all(64);
    Some(
        result
            .dates
            .into_iter()
            .map(|d| d.with_timezone(&Utc))
            .collect(),
    )
}

/// iCalendar date-time (UTC "…Z", zoned with TZID, or floating = local).
fn to_utc(value: &str, tzid: Option<&str>) -> Option<DateTime<Utc>> {
    if let Some(v) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
        return Some(Utc.from_utc_datetime(&naive));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })?;
    match tzid.and_then(|t| t.parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|d| d.with_timezone(&Utc)),
        None => Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|d| d.with_timezone(&Utc)),
    }
}

/// "PT1H30M", "PT45M", "P1D" (the forms calendars actually use).
fn parse_duration(s: &str) -> Option<chrono::Duration> {
    let s = s.strip_prefix('P')?;
    let (days, time) = s.split_once('T').unwrap_or((s, ""));
    let mut secs = 0i64;
    let mut num = String::new();
    for c in days.chars().chain(['|']).chain(time.chars()) {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num.parse().unwrap_or(0);
        num.clear();
        secs += match c {
            'W' => n * 7 * 86_400,
            'D' => n * 86_400,
            'H' => n * 3600,
            'M' => n * 60,
            'S' => n,
            _ => 0,
        };
    }
    (secs > 0).then(|| chrono::Duration::seconds(secs))
}

fn parse_event(ics: &str) -> Option<Event> {
    let mut e = Event::default();
    let mut depth = 0; // skip nested components such as VALARM
    let mut in_event = false;
    for line in unfold(ics) {
        let (name, params, value) = split_line(&line)?;
        match (name.as_str(), value.as_str()) {
            ("BEGIN", "VEVENT") => in_event = true,
            ("END", "VEVENT") => break,
            ("BEGIN", _) if in_event => depth += 1,
            ("END", _) if in_event => depth -= 1,
            _ if !in_event || depth > 0 => {}
            ("UID", v) => e.uid = v.to_owned(),
            ("SUMMARY", v) => e.title = unescape(v),
            ("DTSTART", v) => {
                e.all_day = params.get("VALUE").map(String::as_str) == Some("DATE") || v.len() == 8;
                e.start = Some((v.to_owned(), params.get("TZID").cloned()));
            }
            ("DTEND", v) => e.end = Some((v.to_owned(), params.get("TZID").cloned())),
            ("DURATION", v) => e.duration = Some(v.to_owned()),
            ("RRULE", v) => e.rrule = Some(v.to_owned()),
            ("EXDATE", v) => {
                for d in v.split(',') {
                    e.exdates.push((d.to_owned(), params.get("TZID").cloned()));
                }
            }
            ("RECURRENCE-ID", v) => {
                e.recurrence_id = Some((v.to_owned(), params.get("TZID").cloned()));
            }
            ("STATUS", v) => e.cancelled = v.eq_ignore_ascii_case("CANCELLED"),
            ("ATTENDEE", v) => {
                if params.get("PARTSTAT").map(String::as_str) == Some("DECLINED") {
                    let who = v
                        .trim_start_matches("mailto:")
                        .trim_start_matches("MAILTO:");
                    e.declined_by.push(who.to_lowercase());
                }
            }
            ("X-GOOGLE-CONFERENCE" | "LOCATION" | "DESCRIPTION" | "URL", v)
                if e.join_url.is_none() =>
            {
                e.join_url = find_join_url(&unescape(v));
            }
            _ => {}
        }
    }
    (!e.uid.is_empty()).then_some(e)
}

/// Joins folded lines (continuations start with a space or tab).
fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let raw = raw.trim_end_matches('\r');
        if let Some(rest) = raw.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        if !raw.is_empty() {
            lines.push(raw.to_owned());
        }
    }
    lines
}

/// "NAME;K=V;K2=\"v\":value" -> (NAME, {K: V}, value).
fn split_line(line: &str) -> Option<(String, HashMap<String, String>, String)> {
    let mut in_quotes = false;
    let colon = line.char_indices().find_map(|(i, c)| {
        if c == '"' {
            in_quotes = !in_quotes;
        }
        (c == ':' && !in_quotes).then_some(i)
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_ascii_uppercase(), v.trim_matches('"').to_owned()))
        })
        .collect();
    Some((name, params, value.to_owned()))
}

fn unescape(v: &str) -> String {
    v.replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// First video-call link in some text.
fn find_join_url(text: &str) -> Option<String> {
    const HOSTS: &[&str] = &[
        "meet.google.com/",
        "zoom.us/j/",
        "teams.microsoft.com/l/meetup-join",
    ];
    text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '(' | ')'))
        .filter(|w| w.starts_with("https://"))
        .find(|w| HOSTS.iter().any(|h| w.contains(h)))
        .map(|w| w.trim_end_matches(['.', ',']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(body: &str) -> String {
        format!("BEGIN:VEVENT\r\nUID:u1\r\n{body}\r\nEND:VEVENT\r\n")
    }

    fn day(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, h, 0, 0).unwrap()
    }

    #[test]
    fn single_event_with_meet_link_and_folding() {
        let o = ev("DTSTART:20261022T080000Z\r\nDTEND:20261022T090000Z\r\nSUMMARY:Stand\\, up\r\nDESCRIPTION:Join: https://meet.go\r\n ogle.com/abc-defg-hij\\nThanks");
        let m = expand(&[o], day(0), day(23), None);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].title, "Stand, up");
        assert_eq!(m[0].end - m[0].start, 3600);
        assert_eq!(
            m[0].join_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }

    #[test]
    fn weekly_rule_expands_with_timezone_and_exdate() {
        // Thursdays 15:30 Jakarta (UTC+7) = 08:30 UTC.
        let o = ev("DTSTART;TZID=Asia/Jakarta:20261008T153000\r\nDURATION:PT30M\r\nRRULE:FREQ=WEEKLY;BYDAY=TH\r\nEXDATE;TZID=Asia/Jakarta:20261015T153000\r\nSUMMARY:Weekly");
        let from = Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 31, 0, 0, 0).unwrap();
        let m = expand(&[o], from, to, None);
        let starts: Vec<String> = m
            .iter()
            .map(|m| {
                DateTime::from_timestamp(m.start, 0)
                    .unwrap()
                    .format("%d %H:%M")
                    .to_string()
            })
            .collect();
        assert_eq!(starts, vec!["22 08:30", "29 08:30"]); // 15th excluded
    }

    #[test]
    fn keeps_all_day_flagged_skips_cancelled_and_declined() {
        let all_day = ev("DTSTART;VALUE=DATE:20261022\r\nSUMMARY:Holiday");
        let cancelled =
            ev("DTSTART:20261022T080000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:x").replace("u1", "u2");
        let declined = ev("DTSTART:20261022T090000Z\r\nATTENDEE;CN=Me;PARTSTAT=DECLINED:mailto:me@example.com\r\nSUMMARY:y").replace("u1", "u3");
        let m = expand(
            &[all_day, cancelled, declined],
            day(0),
            day(23),
            Some("me@example.com"),
        );
        assert_eq!(m.len(), 1);
        assert!(m[0].all_day);
        assert_eq!(m[0].title, "Holiday");
    }

    #[test]
    fn edited_occurrence_replaces_the_original() {
        let master = ev("DTSTART:20261021T080000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Daily");
        let moved = ev("RECURRENCE-ID:20261022T080000Z\r\nDTSTART:20261022T100000Z\r\nDURATION:PT1H\r\nSUMMARY:Daily (moved)");
        let m = expand(&[master, moved], day(0), day(23), None);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].title, "Daily (moved)");
    }

    #[test]
    fn durations() {
        assert_eq!(
            parse_duration("PT1H30M"),
            Some(chrono::Duration::minutes(90))
        );
        assert_eq!(parse_duration("P1D"), Some(chrono::Duration::days(1)));
    }
}
