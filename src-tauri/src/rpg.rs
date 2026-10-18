//! RPG layer: keyword -> attribute buckets, EXP, the leveling curve, and ranks.
//!
//! Everything tunable is a constant at the top of this file.
//!
//! * Completing a quest gives `EXP_PER_BUCKET` to every attribute whose keyword
//!   appears in the task text (a task can feed several), or `EXP_UNSORTED`
//!   general EXP if no keyword matches.
//! * Level comes from total EXP on an exponential curve:
//!       exp needed to go from level n to n+1  =  BASE_EXP * GROWTH^(n-1)
//!   With the defaults and ~3 quests a day, E->D takes about a month and
//!   D->C about half a year (see README for the full table).
//! * Each attribute bar has its own level on the same curve.
//! * Task ids are remembered so a task can never be farmed twice.

use crate::{config::load_json, tasks::Task};
use serde::{Deserialize, Serialize};

pub const BASE_EXP: f64 = 400.0;
pub const GROWTH: f64 = 1.10;
pub const EXP_PER_BUCKET: u64 = 20;
pub const EXP_UNSORTED: u64 = 10;
/// Calendar events pay by length: one EXP per `EVENT_MINUTES_PER_EXP` minutes,
/// clamped to `EVENT_MIN_EXP..=EVENT_MAX_EXP`, to EVERY attribute whose keyword the
/// title contains (like tasks). 30 min = 10, 1 h = 20, 2 h = 40, 3 h = 60, 4.5 h+ = 90.
/// An event ticked without ever starting the focus timer pays `UNTRACKED_FACTOR` of that,
/// so using the timer is worth about 67 % more.
pub const EVENT_MINUTES_PER_EXP: f64 = 3.0;
pub const EVENT_MIN_EXP: u64 = 10;
pub const EVENT_MAX_EXP: u64 = 90;
pub const UNTRACKED_FACTOR: f64 = 0.6;
const MAX_LEVEL: u32 = 999;
const MAX_REMEMBERED_TASKS: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bucket {
    Intellect,
    Vitals,
    Perception,
    Strength,
}

impl Bucket {
    /// Bar label shown in the UI.
    pub fn short(self) -> &'static str {
        match self {
            Bucket::Vitals => "HP",
            Bucket::Intellect => "MP",
            Bucket::Perception => "AGI",
            Bucket::Strength => "STR",
        }
    }
}

/// Keyword stems per attribute. A word matches a stem exactly or with a common
/// suffix ("reading", "runs", "studies"), but never as a loose prefix, so
/// "ready" does not count as "read" and "single" does not count as "sing".
const KEYWORDS: [(Bucket, &[&str]); 4] = [
    (Bucket::Intellect, &["math", "quant", "code", "coding", "study", "read"]),
    (Bucket::Vitals, &["gym", "workout", "sleep", "water", "run"]),
    (Bucket::Perception, &["draw", "sing", "language", "journal"]),
    (Bucket::Strength, &["chores", "chore", "clean", "lift", "build"]),
];
const SUFFIXES: [&str; 7] = ["s", "es", "ed", "er", "ers", "ing", "ning"];

fn word_matches(word: &str, stem: &str) -> bool {
    if word == stem {
        return true;
    }
    if let Some(rest) = word.strip_prefix(stem) {
        return SUFFIXES.contains(&rest);
    }
    // study -> studies / studied
    if let Some(base) = stem.strip_suffix('y') {
        if let Some(rest) = word.strip_prefix(base) {
            return rest == "ies" || rest == "ied";
        }
    }
    false
}

pub fn classify(text: &str) -> Vec<Bucket> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    for (bucket, stems) in KEYWORDS {
        if words.iter().any(|w| stems.iter().any(|s| word_matches(w, s))) {
            out.push(bucket);
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RpgState {
    pub general_exp: u64,
    pub intellect: u64,
    pub vitals: u64,
    pub perception: u64,
    pub strength: u64,
    /// Ids of tasks that already paid out EXP.
    pub awarded: Vec<String>,
}

impl RpgState {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        load_json(path)
    }

    pub fn total_exp(&self) -> u64 {
        self.general_exp + self.intellect + self.vitals + self.perception + self.strength
    }

    fn bucket_exp(&self, b: Bucket) -> u64 {
        match b {
            Bucket::Intellect => self.intellect,
            Bucket::Vitals => self.vitals,
            Bucket::Perception => self.perception,
            Bucket::Strength => self.strength,
        }
    }

    fn bucket_mut(&mut self, b: Bucket) -> &mut u64 {
        match b {
            Bucket::Intellect => &mut self.intellect,
            Bucket::Vitals => &mut self.vitals,
            Bucket::Perception => &mut self.perception,
            Bucket::Strength => &mut self.strength,
        }
    }

    /// Pays out EXP for a completed task once. Returns the EXP gained (0 if already paid).
    /// Pays EXP for a finished calendar block (once per `key`). `tracked` = the focus
    /// timer was used on it. Returns the EXP paid in total.
    pub fn award_event(&mut self, key: &str, title: &str, minutes: i64, tracked: bool) -> u64 {
        if self.awarded.iter().any(|k| k == key) {
            return 0;
        }
        let factor = if tracked { 1.0 } else { UNTRACKED_FACTOR };
        let scale = |v: u64| ((v as f64 * factor).round() as u64).max(1);
        let base = event_exp(minutes);
        let buckets = classify(title);
        let gained = if buckets.is_empty() {
            let g = scale((base / 2).max(5));
            self.general_exp += g;
            g
        } else {
            let each = scale(base);
            for b in &buckets {
                *self.bucket_mut(*b) += each;
            }
            each * buckets.len() as u64
        };
        self.awarded.push(key.to_string());
        if self.awarded.len() > MAX_REMEMBERED_TASKS {
            let excess = self.awarded.len() - MAX_REMEMBERED_TASKS;
            self.awarded.drain(..excess);
        }
        gained
    }

    pub fn award(&mut self, task: &Task) -> u64 {
        if self.awarded.contains(&task.id) {
            return 0;
        }
        let text = format!("{} {}", task.title, task.notes.as_deref().unwrap_or(""));
        let buckets = classify(&text);
        let gained = if buckets.is_empty() {
            self.general_exp += EXP_UNSORTED;
            EXP_UNSORTED
        } else {
            for b in &buckets {
                *self.bucket_mut(*b) += EXP_PER_BUCKET;
            }
            EXP_PER_BUCKET * buckets.len() as u64
        };
        self.awarded.push(task.id.clone());
        if self.awarded.len() > MAX_REMEMBERED_TASKS {
            let excess = self.awarded.len() - MAX_REMEMBERED_TASKS;
            self.awarded.drain(..excess);
        }
        gained
    }
}

/// What `RpgState::award` will pay for this task (same rule, no state change).
pub fn preview(task: &Task) -> crate::tasks::Reward {
    let text = format!("{} {}", task.title, task.notes.as_deref().unwrap_or(""));
    let buckets = classify(&text);
    if buckets.is_empty() {
        crate::tasks::Reward { exp: EXP_UNSORTED, tags: vec![] }
    } else {
        crate::tasks::Reward {
            exp: EXP_PER_BUCKET * buckets.len() as u64,
            tags: buckets.iter().map(|b| b.short().to_string()).collect(),
        }
    }
}

/// EXP a block of `minutes` is worth per attribute (before the untracked discount).
pub fn event_exp(minutes: i64) -> u64 {
    ((minutes.max(0) as f64 / EVENT_MINUTES_PER_EXP).round() as u64).clamp(EVENT_MIN_EXP, EVENT_MAX_EXP)
}

/// Preview shown on an event's row: what finishing it WITH the focus timer pays.
pub fn event_preview(title: &str, minutes: i64) -> crate::tasks::Reward {
    let base = event_exp(minutes);
    let buckets = classify(title);
    if buckets.is_empty() {
        crate::tasks::Reward { exp: (base / 2).max(5), tags: vec![] }
    } else {
        crate::tasks::Reward {
            exp: base * buckets.len() as u64,
            tags: buckets.iter().map(|b| b.short().to_string()).collect(),
        }
    }
}

/// EXP required to advance from `level` to `level + 1`.
pub fn exp_needed(level: u32) -> u64 {
    (BASE_EXP * GROWTH.powi(level as i32 - 1)).round() as u64
}

pub struct LevelInfo {
    pub level: u32,
    /// EXP earned inside the current level.
    pub into: u64,
    /// EXP needed to finish the current level.
    pub need: u64,
}

pub fn level_info(total_exp: u64) -> LevelInfo {
    let (mut level, mut left) = (1u32, total_exp);
    while level < MAX_LEVEL && left >= exp_needed(level) {
        left -= exp_needed(level);
        level += 1;
    }
    LevelInfo { level, into: left, need: exp_needed(level) }
}

/// (letter, neon colour, title) for a level.
pub fn rank_for(level: u32) -> (&'static str, &'static str, &'static str) {
    match level {
        0..=4 => ("E", "#00a2ff", "Awakened"),
        5..=14 => ("D", "#00ffcc", "Disciplined"),
        15..=29 => ("C", "#8a2be2", "Relentless"),
        30..=49 => ("B", "#ff00ff", "Elite Player"),
        50..=79 => ("A", "#ff0033", "Crimson Hunter"),
        _ => ("S", "#ffd700", "Shadow Monarch"),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Bar {
    pub key: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub level: u32,
    pub into: u64,
    pub need: u64,
    pub exp: u64,
}

/// Everything the UI needs to draw the Status window.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RpgView {
    pub level: u32,
    pub into: u64,
    pub need: u64,
    pub total_exp: u64,
    pub rank: &'static str,
    pub rank_title: &'static str,
    pub color: &'static str,
    /// HP, MP, AGI, STR
    pub bars: Vec<Bar>,
}

pub fn view(s: &RpgState) -> RpgView {
    let total = s.total_exp();
    let li = level_info(total);
    let (rank, color, rank_title) = rank_for(li.level);
    let bar = |key, name, short, b: Bucket| {
        let exp = s.bucket_exp(b);
        let l = level_info(exp);
        Bar { key, name, short, level: l.level, into: l.into, need: l.need, exp }
    };
    RpgView {
        level: li.level,
        into: li.into,
        need: li.need,
        total_exp: total,
        rank,
        rank_title,
        color,
        bars: vec![
            bar("vitals", "Vitals", "HP", Bucket::Vitals),
            bar("intellect", "Intellect", "MP", Bucket::Intellect),
            bar("perception", "Perception", "AGI", Bucket::Perception),
            bar("strength", "Strength", "STR", Bucket::Strength),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str) -> Task {
        Task {
            id: id.into(),
            title: title.into(),
            status: "completed".into(),
            due: None,
            completed: None,
            notes: None,
            event: None,
            reward: None,
        }
    }

    #[test]
    fn keywords_route_to_the_right_attribute() {
        assert_eq!(classify("Study quant for 1h"), vec![Bucket::Intellect]);
        assert_eq!(classify("Morning run + water"), vec![Bucket::Vitals]);
        assert_eq!(classify("Journal and draw"), vec![Bucket::Perception]);
        assert_eq!(classify("Clean the flat"), vec![Bucket::Strength]);
        assert_eq!(classify("Reading, then gym"), vec![Bucket::Intellect, Bucket::Vitals]);
        assert_eq!(classify("Studies on language"), vec![Bucket::Intellect, Bucket::Perception]);
    }

    #[test]
    fn loose_prefixes_do_not_match() {
        assert!(classify("Get ready for the single track").is_empty());
        assert!(classify("Pay rent").is_empty());
    }

    #[test]
    fn a_task_pays_out_only_once() {
        let mut s = RpgState::default();
        assert_eq!(s.award(&task("a", "gym")), EXP_PER_BUCKET);
        assert_eq!(s.award(&task("a", "gym")), 0);
        assert_eq!(s.vitals, EXP_PER_BUCKET);
        assert_eq!(s.award(&task("b", "pay rent")), EXP_UNSORTED);
        assert_eq!(s.total_exp(), EXP_PER_BUCKET + EXP_UNSORTED);
    }

    #[test]
    fn curve_is_exponential_and_levels_are_consistent() {
        assert!(exp_needed(10) > exp_needed(5) * 3 / 2);
        assert_eq!(level_info(0).level, 1);
        assert_eq!(level_info(exp_needed(1)).level, 2);
        assert_eq!(level_info(exp_needed(1) - 1).level, 1);
    }

    #[test]
    fn rank_thresholds_and_colours() {
        for (lvl, letter, color) in [
            (1, "E", "#00a2ff"),
            (4, "E", "#00a2ff"),
            (5, "D", "#00ffcc"),
            (14, "D", "#00ffcc"),
            (15, "C", "#8a2be2"),
            (29, "C", "#8a2be2"),
            (30, "B", "#ff00ff"),
            (49, "B", "#ff00ff"),
            (50, "A", "#ff0033"),
            (79, "A", "#ff0033"),
            (80, "S", "#ffd700"),
            (200, "S", "#ffd700"),
        ] {
            let (l, c, _) = rank_for(lvl);
            assert_eq!((l, c), (letter, color), "level {lvl}");
        }
    }

    #[test]
    fn preview_matches_what_award_pays() {
        let mut state = RpgState::default();
        for title in ["Study quant and workout", "Call mom", "Clean the flat"] {
            let t = task("x", title);
            let promised = preview(&t).exp;
            let mut fresh = RpgState::default();
            assert_eq!(fresh.award(&t), promised, "{title}");
            state.award(&task(title, title));
        }
        assert_eq!(preview(&task("a", "Study quant and workout")).tags, vec!["MP", "HP"]);
    }

    #[test]
    fn events_pay_by_length_into_the_matching_attribute() {
        assert_eq!(event_exp(30), 10);
        assert_eq!(event_exp(60), 20);
        assert_eq!(event_exp(120), 40);
        assert_eq!(event_exp(5), EVENT_MIN_EXP);
        assert_eq!(event_exp(60 * 12), EVENT_MAX_EXP);

        let mut s = RpgState::default();
        // 2 h of study, focus timer used: 40 into Intellect only
        assert_eq!(s.award_event("event:a", "Study quant", 120, true), 40);
        assert_eq!((s.intellect, s.vitals, s.general_exp), (40, 0, 0));
        // never paid twice
        assert_eq!(s.award_event("event:a", "Study quant", 120, true), 0);
        // untracked = 60 %
        assert_eq!(s.award_event("event:b", "Study math", 120, false), 24);
        // two keywords feed two attributes
        let mut t = RpgState::default();
        assert_eq!(t.award_event("event:c", "Gym then study", 60, true), 40);
        assert_eq!((t.intellect, t.vitals), (20, 20));
        // no keyword: smaller general EXP
        assert_eq!(RpgState::default().award_event("event:d", "Call plumber", 60, true), 10);
        assert_eq!(event_preview("Study", 120).exp, 40);
    }
}
