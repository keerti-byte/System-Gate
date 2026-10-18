//! RPG attributes, exponential levels, and rank colors.
//!
//! Completing a quest awards EXP into keyword buckets. Player level is the
//! sum of every bucket, on a steep exponential curve so rank-ups take months.

use crate::config::{load_json, save_json, Dirs};
use anyhow::Result;
use serde::{Deserialize, Serialize};

const INTELLECT: &[&str] = &["math", "quant", "code", "study", "read"];
const VITALS: &[&str] = &["gym", "workout", "sleep", "water", "run"];
const PERCEPTION: &[&str] = &["draw", "sing", "language", "journal"];
const STRENGTH: &[&str] = &["chores", "clean", "lift", "build"];

/// Base EXP granted to each matching bucket (or to STR if nothing matches).
const AWARD: u64 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rank {
    E,
    D,
    C,
    B,
    A,
    S,
}

impl Rank {
    pub fn from_level(level: u32) -> Self {
        match level {
            0..=4 => Rank::E,
            5..=14 => Rank::D,
            15..=29 => Rank::C,
            30..=49 => Rank::B,
            50..=79 => Rank::A,
            _ => Rank::S,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rank::E => "E-Rank",
            Rank::D => "D-Rank",
            Rank::C => "C-Rank",
            Rank::B => "B-Rank",
            Rank::A => "A-Rank",
            Rank::S => "S-Rank",
        }
    }

    /// Neon accent for the System window at this rank.
    pub fn color(self) -> &'static str {
        match self {
            Rank::E => "#00a2ff",
            Rank::D => "#00ffcc",
            Rank::C => "#8a2be2",
            Rank::B => "#ff00ff",
            Rank::A => "#ff0033",
            Rank::S => "#ffd700",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Rank::E => "Player",
            Rank::D => "Hunter",
            Rank::C => "Assassin",
            Rank::B => "Elite Hunter",
            Rank::A => "National Level",
            Rank::S => "Shadow Monarch",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// INTELLECT — MP
    pub mp: u64,
    /// VITALS — HP
    pub hp: u64,
    /// PERCEPTION — AGI
    pub agi: u64,
    /// STRENGTH — STR
    pub str: u64,
}

impl Stats {
    pub fn total(&self) -> u64 {
        self.mp + self.hp + self.agi + self.str
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Player {
    pub stats: Stats,
    /// Task / event ids that have already granted EXP, so polls do not double-award.
    pub awarded: Vec<String>,
    pub last_rank: Rank,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            stats: Stats::default(),
            awarded: Vec::new(),
            last_rank: Rank::E,
        }
    }
}

/// EXP required to go from `level` to `level + 1`.
/// `xp_to_next(n) = round(80 * 1.15^(n-1))` — slow enough that higher ranks take months.
pub fn xp_to_next(level: u32) -> u64 {
    let level = level.max(1);
    (80.0 * 1.15f64.powi(level as i32 - 1)).round() as u64
}

/// Convert cumulative EXP into a 1-based level and leftover EXP toward the next one.
pub fn level_from_xp(mut xp: u64) -> (u32, u64, u64) {
    let mut level = 1u32;
    loop {
        let need = xp_to_next(level);
        if xp < need || level >= 200 {
            return (level, xp, need);
        }
        xp -= need;
        level += 1;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerView {
    pub level: u32,
    pub xp: u64,
    pub xp_next: u64,
    pub rank: Rank,
    pub rank_label: String,
    pub color: String,
    pub title: String,
    pub hp: u64,
    pub mp: u64,
    pub agi: u64,
    pub str: u64,
    /// 0–100 fill for each bar, relative to the highest stat (min 1).
    pub hp_pct: u8,
    pub mp_pct: u8,
    pub agi_pct: u8,
    pub str_pct: u8,
    pub evolution: Option<String>,
}

impl PlayerView {
    pub fn from_player(p: &Player) -> Self {
        let total = p.stats.total();
        let (level, xp, xp_next) = level_from_xp(total);
        let rank = Rank::from_level(level);
        let peak = p.stats.hp.max(p.stats.mp).max(p.stats.agi).max(p.stats.str).max(1);
        let pct = |v: u64| ((v as f64 / peak as f64) * 100.0).round().clamp(0.0, 100.0) as u8;
        Self {
            level,
            xp,
            xp_next,
            rank,
            rank_label: rank.label().into(),
            color: rank.color().into(),
            title: rank.title().into(),
            hp: p.stats.hp,
            mp: p.stats.mp,
            agi: p.stats.agi,
            str: p.stats.str,
            hp_pct: pct(p.stats.hp),
            mp_pct: pct(p.stats.mp),
            agi_pct: pct(p.stats.agi),
            str_pct: pct(p.stats.str),
            evolution: None,
        }
    }
}

pub fn load(dirs: &Dirs) -> Player {
    load_json(&dirs.player()).unwrap_or_default()
}

pub fn save(dirs: &Dirs, player: &Player) -> Result<()> {
    save_json(&dirs.player(), player)
}

/// Which buckets a quest title feeds. Empty → default to STRENGTH.
pub fn buckets_for(title: &str) -> Vec<&'static str> {
    let hay = title.to_lowercase();
    let mut out = Vec::new();
    if INTELLECT.iter().any(|k| hay.contains(k)) {
        out.push("mp");
    }
    if VITALS.iter().any(|k| hay.contains(k)) {
        out.push("hp");
    }
    if PERCEPTION.iter().any(|k| hay.contains(k)) {
        out.push("agi");
    }
    if STRENGTH.iter().any(|k| hay.contains(k)) {
        out.push("str");
    }
    if out.is_empty() {
        out.push("str");
    }
    out
}

fn apply_buckets(stats: &mut Stats, buckets: &[&str]) {
    for b in buckets {
        match *b {
            "mp" => stats.mp += AWARD,
            "hp" => stats.hp += AWARD,
            "agi" => stats.agi += AWARD,
            _ => stats.str += AWARD,
        }
    }
}

/// Award EXP for a newly completed quest. Returns a view; `evolution` is set
/// when the player crossed a rank threshold.
pub fn award(dirs: &Dirs, id: &str, title: &str) -> Result<PlayerView> {
    let mut p = load(dirs);
    if p.awarded.iter().any(|x| x == id) {
        return Ok(PlayerView::from_player(&p));
    }
    apply_buckets(&mut p.stats, &buckets_for(title));
    p.awarded.push(id.to_string());
    if p.awarded.len() > 4000 {
        let drain = p.awarded.len() - 4000;
        p.awarded.drain(0..drain);
    }
    let mut view = PlayerView::from_player(&p);
    if view.rank != p.last_rank {
        view.evolution = Some(format!("SYSTEM EVOLUTION — {}", view.rank_label));
        p.last_rank = view.rank;
    }
    save(dirs, &p)?;
    Ok(view)
}

/// Catch EXP for tasks that were completed outside the app (browser, phone).
pub fn award_completed(dirs: &Dirs, id: &str, title: &str) -> Result<Option<PlayerView>> {
    let p = load(dirs);
    if p.awarded.iter().any(|x| x == id) {
        return Ok(None);
    }
    Ok(Some(award(dirs, id, title)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_route_to_buckets() {
        assert_eq!(buckets_for("LeetCode study session"), vec!["mp"]);
        assert_eq!(buckets_for("Morning run and gym"), vec!["hp"]);
        assert_eq!(buckets_for("Journal + language class"), vec!["agi"]);
        assert_eq!(buckets_for("Clean kitchen / chores"), vec!["str"]);
        assert_eq!(buckets_for("Buy milk"), vec!["str"]);
        assert_eq!(buckets_for("Read then lift"), vec!["mp", "str"]);
    }

    #[test]
    fn curve_is_exponential() {
        assert_eq!(xp_to_next(1), 80);
        assert!(xp_to_next(10) > xp_to_next(5));
        assert!(xp_to_next(40) > 10_000);
        let (lvl, _, _) = level_from_xp(0);
        assert_eq!(lvl, 1);
        let (lvl, rem, need) = level_from_xp(80);
        assert_eq!(lvl, 2);
        assert_eq!(rem, 0);
        assert!(need > 80);
    }

    #[test]
    fn ranks_match_thresholds() {
        assert_eq!(Rank::from_level(1), Rank::E);
        assert_eq!(Rank::from_level(5), Rank::D);
        assert_eq!(Rank::from_level(15), Rank::C);
        assert_eq!(Rank::from_level(30), Rank::B);
        assert_eq!(Rank::from_level(50), Rank::A);
        assert_eq!(Rank::from_level(80), Rank::S);
        assert_eq!(Rank::S.color(), "#ffd700");
    }
}
