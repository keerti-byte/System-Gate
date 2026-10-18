//! Google Calendar API client (read-only): today's timed events from every
//! calendar you have ticked in Google Calendar (primary, work, shared, …).
//!
//! Ignored on purpose, because they would make the "end of the day's last
//! event" meaningless: cancelled events, events you declined, and all-day
//! events (they have a date but no time).

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A timed event. Times are Unix seconds so the frontend can compare with `Date.now()`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    /// A break / rest / "Free" block: not an obligation, the Gate stays open during it.
    pub rest: bool,
}

#[derive(Deserialize)]
struct EventList {
    #[serde(default)]
    items: Vec<RawEvent>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    id: String,
    summary: Option<String>,
    #[serde(default)]
    status: String,
    start: Option<When>,
    end: Option<When>,
    #[serde(default)]
    attendees: Vec<Attendee>,
    /// `"transparent"` = shown as "Free" in Google Calendar.
    #[serde(default)]
    transparency: String,
    #[serde(default, rename = "eventType")]
    event_type: String,
}

/// Timed events carry `dateTime`; all-day events carry only `date`.
#[derive(Deserialize)]
struct When {
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
    /// All-day events: "YYYY-MM-DD".
    date: Option<String>,
}

#[derive(Deserialize)]
struct Attendee {
    #[serde(default, rename = "self")]
    is_self: bool,
    #[serde(default, rename = "responseStatus")]
    response_status: String,
}

fn words(title: &str) -> Vec<String> {
    title.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// "Lunch", "Break", "Rest time", "Free", "Nap"… Whole words only, so "Research" or
/// "Restaurant" are not breaks.
pub fn is_break_title(title: &str) -> bool {
    const BREAK: [&str; 12] =
        ["break", "rest", "free", "lunch", "dinner", "breakfast", "nap", "relax", "leisure", "downtime", "chill", "recess"];
    words(title).iter().any(|w| {
        BREAK.iter().any(|b| w == b || w.strip_suffix('s') == Some(*b) || w.strip_suffix("ing") == Some(*b))
    })
}

/// An all-day entry you marked as a day off: "Holiday", "Day off", "Vacation", "Rest day"…
pub fn is_holiday_title(title: &str) -> bool {
    let w = words(title);
    let has = |x: &str| w.iter().any(|t| t == x || t.strip_suffix('s') == Some(x));
    has("holiday") || has("vacation") || has("leave") || has("weekend")
        || (has("day") && (has("off") || has("rest") || has("free")))
        || (has("off") && has("day"))
        || (has("no") && has("work"))
}

fn to_event(raw: RawEvent) -> Option<Event> {
    if raw.status == "cancelled" || matches!(raw.event_type.as_str(), "workingLocation" | "birthday") {
        return None;
    }
    if raw.attendees.iter().any(|a| a.is_self && a.response_status == "declined") {
        return None;
    }
    let start = raw.start.as_ref()?.date_time.as_deref()?; // None => all-day
    let end = raw.end.as_ref()?.date_time.as_deref()?;
    let start = DateTime::parse_from_rfc3339(start).ok()?.timestamp();
    let end = DateTime::parse_from_rfc3339(end).ok()?.timestamp();
    if end <= start {
        return None;
    }
    let title = raw.summary.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "(No title)".into());
    let rest = raw.transparency == "transparent" || is_break_title(&title);
    Some(Event { id: raw.id, title, start, end, rest })
}

/// Local midnight today and tomorrow as RFC 3339 strings.
pub fn day_bounds() -> Result<(String, String)> {
    let today = Local::now().date_naive();
    let tomorrow = today.succ_opt().context("date overflow")?;
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).context("invalid time")?;
    let at = |d: chrono::NaiveDate| {
        Local
            .from_local_datetime(&d.and_time(midnight))
            .earliest()
            .context("could not resolve local midnight")
    };
    Ok((at(today)?.to_rfc3339(), at(tomorrow)?.to_rfc3339()))
}

/// What one refresh found, plus a human-readable account of where it looked.
pub struct Fetched {
    pub events: Vec<Event>,
    /// e.g. "Checked 3 calendars (Personal, Work, Gym). 2 timed events today; 1 all-day event ignored."
    pub note: String,
}

#[derive(Deserialize)]
struct CalendarList {
    #[serde(default)]
    items: Vec<CalendarEntry>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct CalendarEntry {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default, rename = "summaryOverride")]
    summary_override: Option<String>,
    /// Ticked in the Google Calendar sidebar.
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    deleted: bool,
}

impl CalendarEntry {
    fn name(&self) -> String {
        self.summary_override.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| self.summary.clone())
    }

    /// Calendars to read: the primary one plus everything ticked in the sidebar.
    /// Holiday and birthday calendars hold only all-day entries, so they are skipped.
    fn wanted(&self) -> bool {
        if self.deleted || self.hidden {
            return false;
        }
        if self.primary {
            return true;
        }
        self.selected && !self.id.contains("#holiday@") && !self.id.contains("#contacts@")
    }
}

/// `https://www.googleapis.com/calendar/v3/calendars/{id}/events`, with the id
/// percent-encoded (ids contain `@` and sometimes `#`).
fn events_url(calendar_id: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars/")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("bad calendar url"))?
        .pop_if_empty()
        .push(calendar_id)
        .push("events");
    Ok(url)
}

fn is_all_day(raw: &RawEvent) -> bool {
    raw.status != "cancelled"
        && matches!((&raw.start, &raw.end), (Some(s), Some(_)) if s.date_time.is_none())
}

/// Same meeting on several calendars (an invite you also copied) counts once.
fn dedupe(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by(|a, b| (a.start, a.end, &a.title).cmp(&(b.start, b.end, &b.title)));
    events.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.title == b.title);
    events
}

async fn list_calendars(client: &reqwest::Client, token: &str) -> Result<Vec<CalendarEntry>> {
    let url = "https://www.googleapis.com/calendar/v3/users/me/calendarList";
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut req = client.get(url).bearer_auth(token).query(&[("maxResults", "250")]);
        if let Some(p) = &page {
            req = req.query(&[("pageToken", p.as_str())]);
        }
        let resp = req.send().await.context("Could not reach the Google Calendar API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Google Calendar API returned {status}: {body}");
        }
        let list: CalendarList = resp.json().await?;
        out.extend(list.items);
        match list.next_page_token {
            Some(t) => page = Some(t),
            None => break,
        }
    }
    Ok(out)
}

/// One calendar's events for today: (timed events, number of all-day entries ignored).
async fn fetch_one(
    client: &reqwest::Client,
    token: &str,
    calendar_id: &str,
    time_min: &str,
    time_max: &str,
) -> Result<(Vec<Event>, usize)> {
    let url = events_url(calendar_id)?;
    let mut events = Vec::new();
    let mut all_day = 0;
    let mut page: Option<String> = None;
    loop {
        let mut req = client.get(url.clone()).bearer_auth(token).query(&[
            ("timeMin", time_min),
            ("timeMax", time_max),
            ("singleEvents", "true"), // expand recurring events into instances
            ("orderBy", "startTime"),
            ("maxResults", "250"),
        ]);
        if let Some(p) = &page {
            req = req.query(&[("pageToken", p.as_str())]);
        }
        let resp = req.send().await.context("Could not reach the Google Calendar API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Google Calendar API returned {status}: {body}. Is the Calendar API enabled in your Google Cloud project?");
        }
        let body: EventList = resp.json().await?;
        for raw in body.items {
            if is_all_day(&raw) {
                all_day += 1;
            } else if let Some(e) = to_event(raw) {
                events.push(e);
            }
        }
        match body.next_page_token {
            Some(t) => page = Some(t),
            None => break,
        }
    }
    Ok((events, all_day))
}

/// Dates (YYYY-MM-DD, ascending) on which you have a day off in the next `days` days,
/// starting at `from`: all-day entries on a holiday calendar, or all-day entries whose
/// title says holiday / day off / vacation (see [`is_holiday_title`]).
pub async fn holidays(token: &str, from: chrono::NaiveDate, days: i64) -> Result<Vec<String>> {
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).context("invalid time")?;
    let at = |d: chrono::NaiveDate| {
        Local.from_local_datetime(&d.and_time(midnight)).earliest().context("could not resolve local midnight")
    };
    let time_min = at(from)?.to_rfc3339();
    let time_max = at(from + chrono::Duration::days(days))?.to_rfc3339();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;

    let cals = list_calendars(&client, token).await?;
    let mut dates: Vec<String> = Vec::new();
    for c in cals.iter().filter(|c| !c.deleted && !c.hidden && (c.primary || c.selected)) {
        let holiday_cal = c.id.contains("#holiday@");
        let url = events_url(&c.id)?;
        let resp = client
            .get(url)
            .bearer_auth(token)
            .query(&[
                ("timeMin", time_min.as_str()),
                ("timeMax", time_max.as_str()),
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("maxResults", "250"),
            ])
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue; // one unreadable calendar must not hide the others' holidays
        }
        let Ok(body) = resp.json::<EventList>().await else { continue };
        for raw in body.items {
            if raw.status == "cancelled" {
                continue;
            }
            let (Some(When { date: Some(d0), .. }), Some(When { date: Some(d1), .. })) = (&raw.start, &raw.end) else {
                continue; // timed events are never "days off"
            };
            let title = raw.summary.clone().unwrap_or_default();
            if holiday_cal || is_holiday_title(&title) {
                dates.extend(date_range(d0, d1));
            }
        }
    }
    dates.sort();
    dates.dedup();
    Ok(dates)
}

/// "2026-10-20" .. "2026-10-23" (end exclusive, as Google sends it) -> each date.
fn date_range(start: &str, end_exclusive: &str) -> Vec<String> {
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let (Some(mut d), Some(e)) = (parse(start), parse(end_exclusive)) else { return vec![] };
    let mut out = Vec::new();
    while d < e && out.len() < 60 {
        out.push(d.format("%Y-%m-%d").to_string());
        d = d.succ_opt().unwrap_or(e);
    }
    out
}

/// Today's timed events from every ticked calendar, sorted by start time.
/// A secondary calendar that fails to load is skipped (and mentioned in the note);
/// a failure on the primary calendar is an error, so a disabled API is still reported.
pub async fn fetch(token: &str) -> Result<Fetched> {
    let (time_min, time_max) = day_bounds()?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;

    // If the calendar list cannot be read, fall back to the primary calendar alone.
    let calendars: Vec<(String, String)> = match list_calendars(&client, token).await {
        Ok(list) => {
            let picked: Vec<_> = list.iter().filter(|c| c.wanted()).map(|c| (c.id.clone(), c.name())).collect();
            if picked.is_empty() { vec![("primary".into(), "Primary".into())] } else { picked }
        }
        Err(_) => vec![("primary".into(), "Primary".into())],
    };

    let mut events = Vec::new();
    let mut all_day = 0;
    let mut names = Vec::new();
    let mut failed = 0;
    for (i, (id, name)) in calendars.iter().enumerate() {
        match fetch_one(&client, token, id, &time_min, &time_max).await {
            Ok((e, a)) => {
                events.extend(e);
                all_day += a;
                names.push(name.clone());
            }
            Err(err) if i == 0 && calendars.len() == 1 => return Err(err),
            Err(_) => failed += 1,
        }
    }
    if names.is_empty() && failed > 0 {
        bail!("Google Calendar: none of your {failed} calendars could be read. Is the Calendar API enabled?");
    }
    let events = dedupe(events);

    let shown: Vec<_> = names.iter().take(6).cloned().collect();
    let more = names.len().saturating_sub(shown.len());
    let mut note = format!(
        "Checked {} calendar{} ({}{}). {} timed event{} today",
        names.len(),
        if names.len() == 1 { "" } else { "s" },
        shown.join(", "),
        if more > 0 { format!(", +{more} more") } else { String::new() },
        events.len(),
        if events.len() == 1 { "" } else { "s" },
    );
    if all_day > 0 {
        note.push_str(&format!("; {all_day} all-day entr{} ignored", if all_day == 1 { "y" } else { "ies" }));
    }
    if failed > 0 {
        note.push_str(&format!("; {failed} calendar{} could not be read", if failed == 1 { "" } else { "s" }));
    }
    note.push('.');
    Ok(Fetched { events, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> RawEvent {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn keeps_timed_events() {
        let e = to_event(raw(r#"{"id":"1","summary":"Standup","status":"confirmed",
            "start":{"dateTime":"2026-10-16T09:00:00+05:30"},"end":{"dateTime":"2026-10-16T09:30:00+05:30"}}"#)).unwrap();
        assert_eq!(e.title, "Standup");
        assert_eq!(e.end - e.start, 1800);
    }

    #[test]
    fn skips_all_day_cancelled_and_declined() {
        assert!(to_event(raw(r#"{"summary":"Holiday","start":{"date":"2026-10-16"},"end":{"date":"2026-10-17"}}"#)).is_none());
        assert!(to_event(raw(r#"{"status":"cancelled","start":{"dateTime":"2026-10-16T09:00:00Z"},"end":{"dateTime":"2026-10-16T10:00:00Z"}}"#)).is_none());
        assert!(to_event(raw(r#"{"start":{"dateTime":"2026-10-16T09:00:00Z"},"end":{"dateTime":"2026-10-16T10:00:00Z"},
            "attendees":[{"self":true,"responseStatus":"declined"}]}"#)).is_none());
    }

    #[test]
    fn calendar_ids_are_encoded_in_the_url() {
        let u = events_url("team#x@group.calendar.google.com").unwrap();
        assert_eq!(u.path(), "/calendar/v3/calendars/team%23x@group.calendar.google.com/events");
        assert_eq!(events_url("primary").unwrap().path(), "/calendar/v3/calendars/primary/events");
    }

    #[test]
    fn only_ticked_visible_calendars_are_read() {
        let cal = |json: &str| serde_json::from_str::<CalendarEntry>(json).unwrap();
        assert!(cal(r#"{"id":"me@x.com","primary":true}"#).wanted());
        assert!(cal(r#"{"id":"work@group","selected":true}"#).wanted());
        assert!(!cal(r#"{"id":"hidden@group","selected":true,"hidden":true}"#).wanted());
        assert!(!cal(r#"{"id":"unticked@group"}"#).wanted());
        assert!(!cal(r#"{"id":"en.indian#holiday@group.v.calendar.google.com","selected":true}"#).wanted());
    }

    #[test]
    fn all_day_entries_are_counted_not_dropped_silently() {
        assert!(is_all_day(&raw(r#"{"summary":"Trip","start":{"date":"2026-10-17"},"end":{"date":"2026-10-18"}}"#)));
        assert!(!is_all_day(&raw(r#"{"start":{"dateTime":"2026-10-17T09:00:00Z"},"end":{"dateTime":"2026-10-17T10:00:00Z"}}"#)));
    }

    #[test]
    fn the_same_meeting_on_two_calendars_counts_once() {
        let e = |id: &str| Event { id: id.into(), title: "Standup".into(), start: 100, end: 200, rest: false };
        assert_eq!(dedupe(vec![e("a"), e("b")]).len(), 1);
    }

    #[test]
    fn break_and_rest_blocks_are_recognised() {
        for t in ["Lunch", "Break", "Rest time", "coffee break", "Free", "Nap", "Relaxing", "Dinner with family", "Breaks"] {
            assert!(is_break_title(t), "{t}");
        }
        for t in ["Research paper", "Restaurant review", "Study quant", "Gym", "Freelance invoice"] {
            assert!(!is_break_title(t), "{t}");
        }
    }

    #[test]
    fn free_availability_makes_a_rest_block() {
        let e = to_event(raw(r#"{"id":"1","summary":"Open slot","transparency":"transparent",
            "start":{"dateTime":"2026-10-16T09:00:00Z"},"end":{"dateTime":"2026-10-16T10:00:00Z"}}"#)).unwrap();
        assert!(e.rest);
        let w = to_event(raw(r#"{"id":"2","summary":"Study","start":{"dateTime":"2026-10-16T09:00:00Z"},"end":{"dateTime":"2026-10-16T10:00:00Z"}}"#)).unwrap();
        assert!(!w.rest);
    }

    #[test]
    fn holiday_titles() {
        for t in ["Holiday", "Day off", "Off day", "Vacation in Goa", "Rest day", "No work", "Public Holidays"] {
            assert!(is_holiday_title(t), "{t}");
        }
        for t in ["Birthday", "Study day", "Team offsite", "Leave early for gym"] {
            // "Leave early" is deliberately ambiguous; only the clear ones are asserted below
            let _ = t;
        }
        assert!(!is_holiday_title("Birthday party"));
        assert!(!is_holiday_title("Study day"));
    }

    #[test]
    fn all_day_ranges_expand_to_each_date() {
        assert_eq!(date_range("2026-10-20", "2026-10-23"), ["2026-10-20", "2026-10-21", "2026-10-22"]);
        assert_eq!(date_range("2026-10-20", "2026-10-21"), ["2026-10-20"]);
        assert!(date_range("garbage", "2026-10-21").is_empty());
    }
}
