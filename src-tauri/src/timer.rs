//! Pomodoro timer. It lives in Rust (not in the page) so it keeps running, and can
//! notify you, while the window is hidden in the tray.
//!
//! Focus -> short break (long break after every `long_break_every` focus sessions) -> idle.
//! Starting it on a quest marks that quest "in progress"; without the timer a quest is
//! simply "not started". Focus time is recorded when a session finishes or is stopped.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Focus,
    ShortBreak,
    LongBreak,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimerState {
    pub phase: Phase,
    pub quest_key: Option<String>,
    pub quest_title: Option<String>,
    pub started_at: i64,
    pub ends_at: i64,
    /// Focus sessions finished in this run (decides when the long break comes).
    pub cycle: u32,
    /// Bumped on every change so a stale sleeping task can tell it was superseded.
    #[serde(skip)]
    pub generation: u64,
}

impl Default for TimerState {
    fn default() -> Self {
        Self { phase: Phase::Idle, quest_key: None, quest_title: None, started_at: 0, ends_at: 0, cycle: 0, generation: 0 }
    }
}

/// What follows a finished focus session.
pub fn after_focus(cycle_after: u32, long_every: u32) -> Phase {
    if cycle_after % long_every.max(1) == 0 {
        Phase::LongBreak
    } else {
        Phase::ShortBreak
    }
}

pub fn minutes_for(phase: Phase, focus: u32, short: u32, long: u32) -> u32 {
    match phase {
        Phase::Focus => focus,
        Phase::ShortBreak => short,
        Phase::LongBreak => long,
        Phase::Idle => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_break_comes_after_every_fourth_focus_session() {
        let seq: Vec<Phase> = (1..=8).map(|n| after_focus(n, 4)).collect();
        assert_eq!(
            seq,
            [Phase::ShortBreak, Phase::ShortBreak, Phase::ShortBreak, Phase::LongBreak, Phase::ShortBreak, Phase::ShortBreak, Phase::ShortBreak, Phase::LongBreak]
        );
        assert_eq!(minutes_for(Phase::Focus, 25, 5, 15), 25);
        assert_eq!(minutes_for(Phase::LongBreak, 25, 5, 15), 15);
        assert_eq!(minutes_for(Phase::Idle, 25, 5, 15), 0);
    }
}
