//! The Dual-Lock rules. Pure functions only (no I/O, no clock), so the logic
//! that decides whether your internet comes back is easy to read and test.
//!
//! The Gate opens only when BOTH hold:
//!   1. Tasks:    no pending task is due today or overdue.
//!   2. Calendar: now >= end of the day's last timed event (none = satisfied).

use crate::{calendar::Event, tasks::Task};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnlockState {
    pub tasks_done: bool,
    pub calendar_done: bool,
    /// `tasks_done && calendar_done`
    pub unlocked: bool,
    pub pending_tasks: usize,
    pub ongoing_event: Option<String>,
    /// End of the day's last event (Unix seconds): when the calendar lock lifts.
    pub unlock_at: Option<i64>,
    pub schedule_start: Option<i64>,
    pub schedule_end: Option<i64>,
    /// 0–100 each. `overall_pct` is the mean, so it reaches 100 only when both do.
    pub tasks_pct: f64,
    pub time_pct: f64,
    pub overall_pct: f64,
}

/// `pending` = unfinished tasks due today or overdue; `done_today` = how many
/// were completed since local midnight; `now` = Unix seconds.
pub fn evaluate(pending: &[Task], done_today: usize, events: &[Event], now: i64) -> UnlockState {
    let pending_tasks = pending.len();
    let total = pending_tasks + done_today;
    let tasks_done = pending_tasks == 0;
    let tasks_pct = if total == 0 { 100.0 } else { done_today as f64 / total as f64 * 100.0 };

    let schedule_start = events.iter().map(|e| e.start).min();
    let schedule_end = events.iter().map(|e| e.end).max();
    let calendar_done = schedule_end.map_or(true, |end| now >= end);
    let time_pct = match (schedule_start, schedule_end) {
        (Some(s), Some(e)) if e > s => ((now - s) as f64 / (e - s) as f64 * 100.0).clamp(0.0, 100.0),
        _ => 100.0,
    };
    let ongoing_event = events.iter().find(|e| e.start <= now && now < e.end).map(|e| e.title.clone());

    UnlockState {
        tasks_done,
        calendar_done,
        unlocked: tasks_done && calendar_done,
        pending_tasks,
        ongoing_event,
        unlock_at: schedule_end,
        schedule_start,
        schedule_end,
        tasks_pct,
        time_pct,
        overall_pct: (tasks_pct + time_pct) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(n: usize) -> Vec<Task> {
        (0..n)
            .map(|i| Task {
                id: i.to_string(),
                title: "t".into(),
                status: "needsAction".into(),
                due: None,
                completed: None,
                notes: None,
                event: None,
                reward: None,
            })
            .collect()
    }
    fn event(title: &str, start: i64, end: i64) -> Event {
        Event { id: title.into(), title: title.into(), start, end }
    }

    #[test]
    fn tasks_done_but_event_ongoing_stays_locked() {
        let s = evaluate(&[], 3, &[event("Workshop", 100, 200)], 150);
        assert!(s.tasks_done && !s.calendar_done && !s.unlocked);
        assert_eq!(s.ongoing_event.as_deref(), Some("Workshop"));
    }

    #[test]
    fn gap_before_the_last_event_still_locks() {
        let events = [event("A", 100, 200), event("B", 300, 400)];
        let s = evaluate(&[], 1, &events, 250);
        assert!(!s.unlocked && s.ongoing_event.is_none());
        assert_eq!(s.unlock_at, Some(400));
    }

    #[test]
    fn events_over_but_task_pending_stays_locked() {
        let s = evaluate(&pending(1), 1, &[event("A", 100, 200)], 500);
        assert!(s.calendar_done && !s.tasks_done && !s.unlocked);
    }

    #[test]
    fn both_conditions_unlock_at_the_exact_end_time() {
        assert!(evaluate(&[], 1, &[event("A", 100, 200)], 200).unlocked);
        assert!(!evaluate(&[], 1, &[event("A", 100, 200)], 199).unlocked);
    }

    #[test]
    fn empty_day_is_unlocked() {
        let s = evaluate(&[], 0, &[], 0);
        assert!(s.unlocked && s.overall_pct == 100.0);
    }

    #[test]
    fn progress_is_the_mean_of_tasks_and_time() {
        let s = evaluate(&pending(1), 1, &[event("A", 0, 100)], 100); // tasks 50%, time 100%
        assert_eq!((s.tasks_pct, s.time_pct, s.overall_pct), (50.0, 100.0, 75.0));
        let s = evaluate(&pending(1), 1, &[event("A", 0, 100)], 50); // 50% and 50%
        assert_eq!(s.overall_pct, 50.0);
    }
}
