//! The day plan and the Gate rule. Pure functions only (no I/O, no clock), so the
//! logic that decides whether your internet is cut is easy to read and test.
//!
//! The Gate is sealed ONLY when something is due right now:
//!
//!   1. a work block is in progress and not finished          -> sealed
//!   2. a block ended unfinished and is not rescheduled yet   -> sealed (reschedule it!)
//!   3. unplanned tasks (no time slot) remain after the evening cutoff -> sealed
//!   4. you finished a block early AND the next block starts right after it
//!      (nothing to rest in)                                   -> sealed until that block ends
//!   otherwise (free time, break/rest/"Free" blocks, finished early with a gap) -> OPEN
//!
//! Blocks are Google Calendar events plus tasks you gave a time slot. Rescheduling is
//! stored locally (`PlanState::moves`): Google Calendar stays read-only and Google Tasks
//! is only ever changed when you tick a task.

use crate::{
    calendar::Event,
    config::load_json,
    rpg,
    tasks::{Reward, Task},
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Two blocks closer than this count as back-to-back (no break between them).
pub const CONTIGUOUS_SLACK_SECS: i64 = 5 * 60;
/// Default length given to an unplanned task when you schedule it.
pub const DEFAULT_TASK_MIN: u32 = 30;

/* ---------------- persisted state ---------------- */

/// Where a quest was moved to. Enough data to recreate it on that day
/// (a moved calendar event is not in Google's list for the new date).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Move {
    /// `"task"` or `"event"`.
    pub kind: String,
    pub title: String,
    /// Local date YYYY-MM-DD.
    pub date: String,
    /// Minutes after local midnight; `None` = "sometime that day" (unplanned).
    pub start_min: Option<u32>,
    pub dur_min: u32,
    /// Moved by the system (missed block / old task), not by you.
    pub auto: bool,
}

/// A block that ended unfinished. Kept until you resolve it or the grace period
/// runs out (this survives midnight and restarts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Missed {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub dur_min: u32,
    pub ended_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanState {
    /// Local date the daily fields below belong to.
    pub day: String,
    /// Calendar events ticked today (`event:<id>`).
    pub done_events: Vec<String>,
    /// Quest key -> unix time the focus timer was first started on it ("in progress").
    pub started: HashMap<String, i64>,
    /// Quest key -> seconds of focus recorded on it.
    pub focus_secs: HashMap<String, u64>,
    pub moves: HashMap<String, Move>,
    pub missed: HashMap<String, Missed>,
    /// "<kind>:<key>:<date>" notifications already sent.
    pub notified: Vec<String>,
    pub sessions_today: u32,
    pub focus_secs_today: u64,
}

impl PlanState {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        load_json(path)
    }

    /// New local day: reset the daily counters, forget old notification markers.
    pub fn roll_day(&mut self, today: &str) {
        if self.day != today {
            self.day = today.to_string();
            self.done_events.clear();
            self.started.clear();
            self.focus_secs.clear();
            self.notified.clear();
            self.sessions_today = 0;
            self.focus_secs_today = 0;
            // moves into the past that were never acted on are dropped (they would reappear as missed)
            self.moves.retain(|_, m| m.date.as_str() >= today);
        }
    }

    /// True the first time `id` is seen today (used so each notification fires once).
    pub fn first_time(&mut self, id: &str) -> bool {
        if self.notified.iter().any(|n| n == id) {
            return false;
        }
        self.notified.push(id.to_string());
        true
    }
}

/* ---------------- items ---------------- */

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    /// `task:<id>` or `event:<id>`.
    pub key: String,
    pub id: String,
    pub kind: &'static str,
    pub title: String,
    /// Unix seconds. `None` = unplanned (no time slot).
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// `upcoming | active | missed | floating | done | rest`
    pub status: &'static str,
    /// Break / rest / "Free": never an obligation, cannot be ticked.
    pub rest: bool,
    pub done: bool,
    /// The focus timer has been used on it.
    pub in_progress: bool,
    pub focus_secs: u64,
    /// Due date (tasks only).
    pub due: Option<String>,
    pub overdue: bool,
    /// Placed here by a reschedule.
    pub moved: bool,
    pub dur_min: u32,
    pub reward: Option<Reward>,
}

pub struct Ctx<'a> {
    pub today: &'a str,
    pub now: i64,
    /// Local midnight today (Unix seconds).
    pub midnight: i64,
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn status_of(start: Option<i64>, end: Option<i64>, done: bool, rest: bool, now: i64) -> &'static str {
    if rest {
        return "rest";
    }
    if done {
        return "done";
    }
    match (start, end) {
        (Some(s), Some(e)) => {
            if now < s {
                "upcoming"
            } else if now < e {
                "active"
            } else {
                "missed"
            }
        }
        _ => "floating",
    }
}

fn slot_of(ctx: &Ctx, m: &Move) -> (Option<i64>, Option<i64>) {
    match m.start_min {
        Some(sm) => {
            let s = ctx.midnight + sm as i64 * 60;
            (Some(s), Some(s + m.dur_min as i64 * 60))
        }
        None => (None, None),
    }
}

/// Builds today's items from Google Tasks (pending), tasks finished today (only the timed
/// ones matter, for the "finished early" rule), calendar events and your reschedules.
pub fn build(ctx: &Ctx, pending: &[Task], done_tasks: &[Task], events: &[Event], st: &PlanState) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut owned_events: HashSet<String> = HashSet::new();

    let push_task = |t: &Task, done: bool, items: &mut Vec<Item>, owned: &mut HashSet<String>| {
        let key = format!("task:{}", t.id);
        let twin = events.iter().find(|e| !e.rest && norm(&e.title) == norm(&t.title));
        if let Some(e) = twin {
            owned.insert(e.id.clone()); // the task owns its calendar twin (no duplicate row)
        }
        let mv = st.moves.get(&key);
        if let Some(m) = mv {
            if m.date.as_str() > ctx.today {
                return; // moved to a later day
            }
        }
        let (start, end, moved) = match mv {
            Some(m) => {
                let (s, e) = slot_of(ctx, m);
                (s, e, true)
            }
            None => match twin {
                Some(e) => (Some(e.start), Some(e.end), false),
                None => (None, None, false),
            },
        };
        if done && start.is_none() {
            return; // finished unplanned tasks are only counted, never listed
        }
        let due = t.due.as_deref().and_then(|d| d.get(..10)).map(String::from);
        let overdue = due.as_deref().map_or(false, |d| d < ctx.today);
        let dur_min = match (start, end) {
            (Some(s), Some(e)) => ((e - s) / 60).max(1) as u32,
            _ => DEFAULT_TASK_MIN,
        };
        items.push(Item {
            key: key.clone(),
            id: t.id.clone(),
            kind: "task",
            title: t.title.trim().to_string(),
            start,
            end,
            status: status_of(start, end, done, false, ctx.now),
            rest: false,
            done,
            in_progress: !done && st.started.contains_key(&key),
            focus_secs: st.focus_secs.get(&key).copied().unwrap_or(0),
            due,
            overdue,
            moved,
            dur_min,
            reward: t.reward.clone().or_else(|| Some(rpg::preview(t))),
        });
    };
    for t in pending {
        push_task(t, false, &mut items, &mut owned_events);
    }
    for t in done_tasks {
        push_task(t, true, &mut items, &mut owned_events);
    }

    for e in events {
        if owned_events.contains(&e.id) {
            continue;
        }
        let key = format!("event:{}", e.id);
        let mv = if e.rest { None } else { st.moves.get(&key) };
        if let Some(m) = mv {
            if m.date.as_str() > ctx.today {
                continue; // moved to a later day
            }
        }
        let (start, end, moved) = match mv {
            Some(m) => {
                let (s, en) = slot_of(ctx, m);
                (s, en, true)
            }
            None => (Some(e.start), Some(e.end), false),
        };
        let done = st.done_events.contains(&key);
        let dur_min = match (start, end) {
            (Some(s), Some(en)) => ((en - s) / 60).max(1) as u32,
            _ => DEFAULT_TASK_MIN,
        };
        items.push(Item {
            key: key.clone(),
            id: e.id.clone(),
            kind: "event",
            title: e.title.clone(),
            start,
            end,
            status: status_of(start, end, done, e.rest, ctx.now),
            rest: e.rest,
            done,
            in_progress: !done && !e.rest && st.started.contains_key(&key),
            focus_secs: st.focus_secs.get(&key).copied().unwrap_or(0),
            due: None,
            overdue: false,
            moved,
            dur_min,
            reward: (!e.rest).then(|| rpg::event_preview(&e.title, dur_min as i64)),
        });
    }

    // Events rescheduled onto today from another day are not in today's Google list.
    for (key, m) in &st.moves {
        if m.kind != "event" || m.date.as_str() > ctx.today || items.iter().any(|i| &i.key == key) {
            continue;
        }
        if events.iter().any(|e| format!("event:{}", e.id) == *key) {
            continue;
        }
        let (start, end) = slot_of(ctx, m);
        let done = st.done_events.contains(key);
        items.push(Item {
            key: key.clone(),
            id: key.trim_start_matches("event:").to_string(),
            kind: "event",
            title: m.title.clone(),
            start,
            end,
            status: status_of(start, end, done, false, ctx.now),
            rest: false,
            done,
            in_progress: !done && st.started.contains_key(key),
            focus_secs: st.focus_secs.get(key).copied().unwrap_or(0),
            due: None,
            overdue: false,
            moved: true,
            dur_min: m.dur_min,
            reward: Some(rpg::event_preview(&m.title, m.dur_min as i64)),
        });
    }

    items.sort_by(|a, b| (a.start.unwrap_or(i64::MAX), &a.title).cmp(&(b.start.unwrap_or(i64::MAX), &b.title)));
    items
}

/// Quests rescheduled to a later day (for the "Rescheduled" list).
pub fn moved_away(st: &PlanState, today: &str) -> Vec<(String, Move)> {
    let mut v: Vec<_> = st.moves.iter().filter(|(_, m)| m.date.as_str() > today).map(|(k, m)| (k.clone(), m.clone())).collect();
    v.sort_by(|a, b| (&a.1.date, &a.1.title).cmp(&(&b.1.date, &b.1.title)));
    v
}

/* ---------------- the Gate rule ---------------- */

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Decision {
    pub sealed: bool,
    /// `active | missed | unplanned | chain | free`
    pub kind: &'static str,
    pub reason: String,
    /// Title of the quest the reason is about.
    pub title: Option<String>,
    /// When this decision may next change on its own (Unix seconds).
    pub until: Option<i64>,
    /// When the current period began (for the progress ring).
    pub since: i64,
    pub unplanned: usize,
    pub missed: usize,
    pub next_title: Option<String>,
}

/// `lock_from_min`: unplanned tasks lock the Gate from this minute of the day (e.g. 1080 = 6:00 PM).
pub fn decide(items: &[Item], now: i64, midnight: i64, lock_from_min: u32, grace_secs: i64) -> Decision {
    let work = || items.iter().filter(|i| !i.rest && !i.done);
    let active: Vec<&Item> = work().filter(|i| i.status == "active").collect();
    let missed: Vec<&Item> = work().filter(|i| i.status == "missed").collect();
    let floating: Vec<&Item> = work().filter(|i| i.status == "floating").collect();
    let lock_ts = midnight + lock_from_min as i64 * 60;
    let upcoming = work().filter(|i| i.status == "upcoming").min_by_key(|i| i.start.unwrap_or(i64::MAX));
    let next_start = upcoming.and_then(|i| i.start);
    let since_free = items.iter().filter_map(|i| i.end).filter(|e| *e <= now).max().unwrap_or(midnight);

    let base = |sealed, kind, reason: String, title: Option<String>, until, since| Decision {
        sealed,
        kind,
        reason,
        title,
        until,
        since,
        unplanned: floating.len(),
        missed: missed.len(),
        next_title: upcoming.map(|i| i.title.clone()),
    };

    // 1. a block is in progress and not finished
    if let Some(a) = active.iter().min_by_key(|i| i.end.unwrap_or(i64::MAX)) {
        let since = active.iter().filter_map(|i| i.start).max().unwrap_or(midnight);
        return base(true, "active", format!("{} is in progress", a.title), Some(a.title.clone()), a.end, since);
    }
    // 2. a block ended unfinished and is not rescheduled yet
    if let Some(m) = missed.iter().min_by_key(|i| i.end.unwrap_or(i64::MAX)) {
        let until = m.end.map(|e| e + grace_secs);
        let reason = format!("{} was not finished. Complete it or reschedule it", m.title);
        return base(true, "missed", reason, Some(m.title.clone()), until, m.end.unwrap_or(midnight));
    }
    // 3. unplanned tasks after the evening cutoff
    if !floating.is_empty() && now >= lock_ts {
        let reason = format!(
            "{} unplanned task{} left. Do {} or give {} a time slot",
            floating.len(),
            if floating.len() == 1 { "" } else { "s" },
            if floating.len() == 1 { "it" } else { "them" },
            if floating.len() == 1 { "it" } else { "them" },
        );
        return base(true, "unplanned", reason, Some(floating[0].title.clone()), None, lock_ts);
    }
    // 4. finished early, but the next block starts right after: nothing to rest in
    if let Some(b) = items
        .iter()
        .filter(|i| !i.rest && i.done)
        .find(|i| matches!((i.start, i.end), (Some(s), Some(e)) if s <= now && now < e))
    {
        let b_end = b.end.unwrap_or(now);
        if let Some(ns) = next_start {
            if ns <= b_end + CONTIGUOUS_SLACK_SECS {
                let reason = format!("{} starts right after. Keep going", upcoming.map(|i| i.title.as_str()).unwrap_or("The next quest"));
                return base(true, "chain", reason, upcoming.map(|i| i.title.clone()), Some(b_end), b.start.unwrap_or(midnight));
            }
        }
    }
    // 5. free time
    let mut until = next_start;
    if !floating.is_empty() && now < lock_ts {
        until = Some(until.map_or(lock_ts, |u| u.min(lock_ts)));
    }
    let reason = match (upcoming, items.iter().any(|i| i.done && !i.rest)) {
        (Some(n), _) => format!("Free until {}", n.title),
        (None, true) => "All planned quests are done".to_string(),
        (None, false) => "Nothing planned right now".to_string(),
    };
    base(false, "free", reason, None, until, since_free)
}

/// Blocks that have been unresolved for longer than the grace period.
pub fn due_for_auto_shift(missed: &HashMap<String, Missed>, now: i64, grace_secs: i64) -> Vec<Missed> {
    let mut v: Vec<Missed> = missed.values().filter(|m| m.ended_at + grace_secs <= now).cloned().collect();
    v.sort_by_key(|m| m.ended_at);
    v
}

/// Missed-block records for every currently missed item (so they survive a restart / midnight).
pub fn missed_records(items: &[Item]) -> Vec<Missed> {
    items
        .iter()
        .filter(|i| i.status == "missed")
        .map(|i| Missed { key: i.key.clone(), kind: i.kind.to_string(), title: i.title.clone(), dur_min: i.dur_min, ended_at: i.end.unwrap_or(0) })
        .collect()
}

/// First holiday strictly after `today` (`holidays` sorted ascending, YYYY-MM-DD).
pub fn first_holiday_after(holidays: &[String], today: &str) -> Option<String> {
    holidays.iter().find(|d| d.as_str() > today).cloned()
}

/// Checks a reschedule request. `now_min` = minutes since local midnight right now.
pub fn validate_move(date: &str, start_min: Option<u32>, dur_min: u32, today: &str, now_min: u32) -> Result<()> {
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        bail!("That date is not valid.");
    }
    if date < today {
        bail!("Pick today or a later day.");
    }
    if !(5..=720).contains(&dur_min) {
        bail!("Length must be between 5 minutes and 12 hours.");
    }
    if let Some(sm) = start_min {
        if sm >= 1440 {
            bail!("That start time is not valid.");
        }
        if date == today && sm <= now_min {
            bail!("That time has already passed. Pick a later time.");
        }
    }
    Ok(())
}

/// "18:00" -> 1080. Falls back to 18:00 on bad input.
pub fn parse_hhmm(s: &str) -> u32 {
    let mut it = s.trim().split(':');
    match (it.next().and_then(|h| h.parse::<u32>().ok()), it.next().and_then(|m| m.parse::<u32>().ok())) {
        (Some(h), Some(m)) if h < 24 && m < 60 => h * 60 + m,
        _ => 18 * 60,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 1_000_000; // "midnight"
    const H: i64 = 3600;
    const T: &str = "2026-10-17";
    const LOCK: u32 = 18 * 60;
    const GRACE: i64 = 30 * 60;

    fn ev(id: &str, title: &str, from_h: f64, to_h: f64) -> Event {
        Event { id: id.into(), title: title.into(), start: M + (from_h * H as f64) as i64, end: M + (to_h * H as f64) as i64, rest: false }
    }
    fn rest(id: &str, title: &str, from_h: f64, to_h: f64) -> Event {
        Event { rest: true, ..ev(id, title, from_h, to_h) }
    }
    fn task(id: &str, title: &str) -> Task {
        Task { id: id.into(), title: title.into(), status: "needsAction".into(), due: Some(format!("{T}T00:00:00.000Z")), completed: None, notes: None, event: None, reward: None }
    }
    fn at(h: f64) -> i64 {
        M + (h * H as f64) as i64
    }
    fn items(events: &[Event], tasks: &[Task], st: &PlanState, now_h: f64) -> Vec<Item> {
        build(&Ctx { today: T, now: at(now_h), midnight: M }, tasks, &[], events, st)
    }
    fn gate(events: &[Event], tasks: &[Task], st: &PlanState, now_h: f64) -> Decision {
        decide(&items(events, tasks, st, now_h), at(now_h), M, LOCK, GRACE)
    }
    fn done(keys: &[&str]) -> PlanState {
        PlanState { done_events: keys.iter().map(|k| k.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn nothing_planned_means_open() {
        let d = gate(&[], &[], &PlanState::default(), 11.0);
        assert!(!d.sealed);
        assert_eq!(d.kind, "free");
    }

    #[test]
    fn sealed_only_while_a_block_is_active() {
        let e = [ev("a", "Study quant", 9.0, 11.0)];
        assert!(!gate(&e, &[], &PlanState::default(), 8.0).sealed, "before it starts: free");
        let d = gate(&e, &[], &PlanState::default(), 9.5);
        assert!(d.sealed && d.kind == "active");
        assert_eq!(d.until, Some(at(11.0)));
    }

    #[test]
    fn free_time_between_blocks_is_open() {
        let e = [ev("a", "Study", 9.0, 10.0), ev("b", "Gym", 14.0, 15.0)];
        let st = done(&["event:a"]);
        let d = gate(&e, &[], &st, 12.0);
        assert!(!d.sealed);
        assert_eq!(d.until, Some(at(14.0)), "open until the next block starts");
        assert_eq!(d.next_title.as_deref(), Some("Gym"));
    }

    #[test]
    fn break_and_free_blocks_never_seal() {
        let e = [ev("a", "Study", 9.0, 10.0), rest("r", "Lunch break", 10.0, 11.0), ev("b", "Gym", 11.0, 12.0)];
        let st = done(&["event:a"]);
        assert!(!gate(&e, &[], &st, 10.5).sealed);
        assert!(gate(&e, &[], &st, 11.5).sealed);
    }

    // The user's scenario: task 1, then a break, then task 2.
    #[test]
    fn finishing_early_before_a_break_lifts_the_gate_until_task_two_starts() {
        let e = [ev("t1", "Task 1", 9.0, 11.0), rest("br", "Break", 11.0, 12.0), ev("t2", "Task 2", 12.0, 13.0)];
        // 09:30, task 1 not done yet: sealed
        assert!(gate(&e, &[], &PlanState::default(), 9.5).sealed);
        // 09:30, task 1 finished early: open until task 2 at 12:00
        let st = done(&["event:t1"]);
        let d = gate(&e, &[], &st, 9.5);
        assert!(!d.sealed);
        assert_eq!(d.until, Some(at(12.0)));
        // task 2 starts: sealed again
        assert!(gate(&e, &[], &st, 12.0).sealed);
    }

    // "ONLY IF I HAVE NOTHING TO DO AFTER THAT": back-to-back blocks stay sealed.
    #[test]
    fn finishing_early_when_the_next_block_follows_immediately_stays_sealed() {
        let e = [ev("t1", "Task 1", 9.0, 10.0), ev("t2", "Task 2", 10.0, 11.0)];
        let st = done(&["event:t1"]);
        let d = gate(&e, &[], &st, 9.5);
        assert!(d.sealed && d.kind == "chain");
        assert_eq!(d.until, Some(at(10.0)));
        // and task 2 then takes over
        assert!(gate(&e, &[], &st, 10.2).sealed);
        // once task 2 is done too, free
        assert!(!gate(&e, &[], &done(&["event:t1", "event:t2"]), 10.5).sealed);
    }

    #[test]
    fn a_block_that_ended_unfinished_keeps_the_gate_sealed_until_resolved() {
        let e = [ev("a", "Study", 9.0, 10.0)];
        let d = gate(&e, &[], &PlanState::default(), 10.2);
        assert!(d.sealed && d.kind == "missed");
        assert_eq!(d.until, Some(at(10.0) + GRACE), "the auto-shift moment");
        // rescheduled to tomorrow: nothing left today
        let mut st = PlanState::default();
        st.moves.insert("event:a".into(), Move { kind: "event".into(), title: "Study".into(), date: "2026-10-18".into(), start_min: Some(600), dur_min: 60, auto: false });
        assert!(!gate(&e, &[], &st, 10.2).sealed);
    }

    #[test]
    fn unplanned_tasks_only_lock_after_the_evening_cutoff() {
        let t = [task("1", "Call mom")];
        assert!(!gate(&[], &t, &PlanState::default(), 12.0).sealed);
        let d = gate(&[], &t, &PlanState::default(), 12.0);
        assert_eq!(d.until, Some(M + LOCK as i64 * 60), "open until the cutoff");
        let d = gate(&[], &t, &PlanState::default(), 18.5);
        assert!(d.sealed && d.kind == "unplanned");
    }

    #[test]
    fn planning_a_task_moves_it_into_a_slot_instead_of_the_evening_lock() {
        let t = [task("1", "Call mom")];
        let mut st = PlanState::default();
        st.moves.insert("task:1".into(), Move { kind: "task".into(), title: "Call mom".into(), date: T.into(), start_min: Some(14 * 60), dur_min: 30, auto: false });
        assert!(!gate(&[], &t, &st, 12.0).sealed);
        assert!(gate(&[], &t, &st, 14.1).sealed);
        // a planned task that was not done by the end of its slot is a missed block, not an evening lock
        let d = gate(&[], &t, &st, 15.0);
        assert!(d.sealed && d.kind == "missed");
    }

    #[test]
    fn a_task_owns_its_calendar_twin() {
        let e = [ev("e1", "Deep work", 14.0, 16.0)];
        let t = [task("1", "deep  work")];
        let it = items(&e, &t, &PlanState::default(), 9.0);
        assert_eq!(it.len(), 1, "no duplicate row");
        assert_eq!((it[0].kind, it[0].start), ("task", Some(at(14.0))));
    }

    #[test]
    fn a_finished_timed_task_still_counts_for_the_back_to_back_rule() {
        let e = [ev("e1", "Task 1", 9.0, 10.0), ev("e2", "Task 2", 10.0, 11.0)];
        let finished = [task("1", "Task 1")];
        let it = build(&Ctx { today: T, now: at(9.5), midnight: M }, &[], &finished, &e, &PlanState::default());
        let d = decide(&it, at(9.5), M, LOCK, GRACE);
        assert!(d.sealed && d.kind == "chain");
    }

    #[test]
    fn moved_away_items_vanish_today_and_come_back_on_their_day() {
        let e = [ev("a", "Study", 9.0, 10.0)];
        let mut st = PlanState::default();
        st.moves.insert("event:a".into(), Move { kind: "event".into(), title: "Study".into(), date: "2026-10-20".into(), start_min: Some(900), dur_min: 60, auto: true });
        assert!(items(&e, &[], &st, 9.5).is_empty());
        assert_eq!(moved_away(&st, T).len(), 1);
        // on the 20th the event is not in Google's list for that day, but the move recreates it
        let ctx = Ctx { today: "2026-10-20", now: M, midnight: M };
        let it = build(&ctx, &[], &[], &[], &st);
        assert_eq!(it.len(), 1);
        assert_eq!((it[0].kind, it[0].moved, it[0].start), ("event", true, Some(M + 900 * 60)));
    }

    #[test]
    fn done_events_are_marked_done_and_rest_blocks_are_rest() {
        let e = [ev("a", "Study", 9.0, 10.0), rest("r", "Break", 10.0, 10.5)];
        let it = items(&e, &[], &done(&["event:a"]), 9.5);
        assert_eq!(it[0].status, "done");
        assert_eq!(it[1].status, "rest");
        assert!(it[1].reward.is_none());
    }

    #[test]
    fn timer_use_marks_a_quest_in_progress() {
        let e = [ev("a", "Study", 9.0, 10.0)];
        let mut st = PlanState::default();
        assert!(!items(&e, &[], &st, 9.5)[0].in_progress);
        st.started.insert("event:a".into(), at(9.1));
        assert!(items(&e, &[], &st, 9.5)[0].in_progress);
    }

    #[test]
    fn auto_shift_waits_for_the_grace_period() {
        let mut missed = HashMap::new();
        missed.insert("event:a".to_string(), Missed { key: "event:a".into(), kind: "event".into(), title: "Study".into(), dur_min: 60, ended_at: 1000 });
        assert!(due_for_auto_shift(&missed, 1000 + GRACE - 1, GRACE).is_empty());
        assert_eq!(due_for_auto_shift(&missed, 1000 + GRACE, GRACE).len(), 1);
    }

    #[test]
    fn next_holiday_is_strictly_after_today() {
        let h = vec!["2026-10-17".to_string(), "2026-10-25".to_string(), "2026-11-01".to_string()];
        assert_eq!(first_holiday_after(&h, "2026-10-17").as_deref(), Some("2026-10-25"));
        assert_eq!(first_holiday_after(&h, "2026-11-01"), None);
    }

    #[test]
    fn reschedule_requests_are_validated() {
        assert!(validate_move("2026-10-18", Some(600), 60, T, 700).is_ok());
        assert!(validate_move("2026-10-17", Some(800), 60, T, 700).is_ok());
        assert!(validate_move("2026-10-17", Some(600), 60, T, 700).is_err(), "time already passed");
        assert!(validate_move("2026-10-16", None, 60, T, 700).is_err(), "past day");
        assert!(validate_move("nope", None, 60, T, 700).is_err());
        assert!(validate_move("2026-10-18", Some(1500), 60, T, 700).is_err());
        assert!(validate_move("2026-10-18", None, 2, T, 700).is_err());
        assert_eq!(parse_hhmm("18:00"), 1080);
        assert_eq!(parse_hhmm("7:05"), 425);
        assert_eq!(parse_hhmm("junk"), 1080);
    }

    #[test]
    fn roll_day_resets_daily_counters_but_keeps_future_moves() {
        let mut st = PlanState { day: "2026-10-16".into(), sessions_today: 3, ..Default::default() };
        st.done_events.push("event:a".into());
        st.moves.insert("task:x".into(), Move { kind: "task".into(), title: "x".into(), date: "2026-10-20".into(), start_min: None, dur_min: 30, auto: false });
        st.moves.insert("task:old".into(), Move { kind: "task".into(), title: "old".into(), date: "2026-10-10".into(), start_min: None, dur_min: 30, auto: false });
        st.roll_day(T);
        assert!(st.done_events.is_empty());
        assert_eq!(st.sessions_today, 0);
        assert!(st.moves.contains_key("task:x") && !st.moves.contains_key("task:old"));
        assert!(st.first_time("a") && !st.first_time("a"));
    }
}
