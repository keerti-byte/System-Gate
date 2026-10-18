//! Orchestration. The Gate is closed whenever something is due, open in free time.
//!
//! A supervisor loop starts with the app (which itself starts at OS login) and never
//! stops. Every tick it fetches Tasks + Calendar, builds the day plan and asks
//! `plan::decide` whether the Gate must be sealed right now:
//!
//!   * sealed   + nothing due        -> release()   (hosts/apps restored)
//!   * unsealed + something due      -> engage()    (hosts/apps sealed)
//!
//! Also handled here: ticking tasks (Google PATCH) and calendar events (local), EXP,
//! rescheduling, the auto-shift of unresolved blocks to your next holiday, the Pomodoro
//! timer, and desktop notifications.
//!
//! There is no "begin" button. Escape hatch: Emergency release ("I SURRENDER"), which
//! keeps auto-enforcement off until local midnight.
//! Fail-closed on unlock: if an API call fails, the Gate stays as it is.

use crate::{
    apps, auth, calendar,
    config::{load_json, save_json, Config, Dirs, LockState},
    hosts, notify,
    plan::{self, Decision, Item, Move, PlanState},
    privilege,
    privilege::Script,
    rpg::{self, RpgState, RpgView},
    tasks::{self, Task},
    timer::{self, Phase, TimerState},
};
use anyhow::{anyhow, bail, Result};
use chrono::{Local, NaiveDate, TimeZone};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};

/// If the admin prompt is cancelled, wait this long before asking again.
const ENGAGE_RETRY_SECS: i64 = 600;
/// How far ahead (days) to look for your next holiday.
const HOLIDAY_LOOKAHEAD_DAYS: i64 = 120;
/// Unplanned tasks overdue by more than this are left alone instead of auto-shifted.
const AUTO_SHIFT_MAX_AGE_DAYS: i64 = 7;
/// At most this many old tasks are auto-shifted per refresh (never flood one holiday).
const AUTO_SHIFT_MAX_PER_PASS: usize = 8;
/// "Starting soon" notice this many seconds before a block.
const SOON_SECS: i64 = 5 * 60;

/// A quest rescheduled to a later day.
#[derive(Debug, Clone, Serialize)]
pub struct MovedView {
    pub key: String,
    pub title: String,
    pub date: String,
    pub start_min: Option<u32>,
    pub auto: bool,
}

/// Everything the UI needs to draw. Emitted to the frontend as `gate-status`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub locked: bool,
    pub authorized: bool,
    /// Signed-in Google address (display only).
    pub account: Option<String>,
    /// Emergency release is in effect for today.
    pub suppressed: bool,
    /// Today's plan: quests, planned blocks and break/rest blocks.
    pub items: Vec<Item>,
    /// Quests rescheduled to a later day.
    pub moved: Vec<MovedView>,
    /// Quests finished today (tasks + events).
    pub done_today: usize,
    /// Why the Gate is sealed / open right now, and until when.
    pub decision: Option<Decision>,
    pub next_holiday: Option<String>,
    pub timer: TimerState,
    pub sessions_today: u32,
    pub focus_secs_today: u64,
    /// Which calendars were read and what was found (shown on the Schedule tab).
    pub calendar_note: Option<String>,
    pub rpg: RpgView,
    pub last_checked: Option<u64>,
    pub error: Option<String>,
    pub os: String,
}

#[derive(Default)]
pub struct HolidayCache {
    pub dates: Vec<String>,
    pub tried_at: i64,
}

pub struct Shared {
    pub dirs: Dirs,
    pub config: Mutex<Config>,
    pub lock: Mutex<LockState>,
    pub rpg: Mutex<RpgState>,
    pub plan: Mutex<PlanState>,
    pub timer: Mutex<TimerState>,
    pub holidays: Mutex<HolidayCache>,
    pub snapshot: Mutex<Snapshot>,
    /// Serialises engage/release so two elevation prompts never overlap.
    pub busy: tokio::sync::Mutex<()>,
    pub supervisor_running: AtomicBool,
    pub engage_retry_after: AtomicI64,
}

impl Shared {
    pub fn load(dirs: Dirs) -> Result<Self> {
        let mut config: Config = load_json(&dirs.config())?;
        let _ = config.sanitize();
        let lock: LockState = load_json(&dirs.state())?;
        let rpg = RpgState::load(&dirs.rpg())?;
        // A damaged plan file must never stop the app (and so the Gate) from starting.
        let mut plan = PlanState::load(&dirs.plan()).unwrap_or_default();
        plan.roll_day(&today_str());
        let snapshot = Snapshot {
            locked: lock.locked,
            rpg: rpg::view(&rpg),
            os: std::env::consts::OS.to_string(),
            ..Default::default()
        };
        Ok(Self {
            dirs,
            config: Mutex::new(config),
            lock: Mutex::new(lock),
            rpg: Mutex::new(rpg),
            plan: Mutex::new(plan),
            timer: Mutex::new(TimerState::default()),
            holidays: Mutex::new(HolidayCache::default()),
            snapshot: Mutex::new(snapshot),
            busy: tokio::sync::Mutex::new(()),
            supervisor_running: AtomicBool::new(false),
            engage_retry_after: AtomicI64::new(0),
        })
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn today_str() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn local_midnight() -> i64 {
    Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|n| Local.from_local_datetime(&n).earliest())
        .map(|d| d.timestamp())
        .unwrap_or_else(|| now_secs() - now_secs() % 86400)
}

fn tomorrow_str() -> String {
    Local::now().date_naive().succ_opt().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(today_str)
}

fn is_locked(app: &AppHandle) -> bool {
    app.state::<Shared>().lock.lock().unwrap().locked
}

/// Mutates the snapshot, refreshes derived fields and pushes it to the frontend.
/// Lock order is always snapshot -> (lock | rpg | plan | timer); never take those while holding the snapshot elsewhere.
fn publish(app: &AppHandle, f: impl FnOnce(&mut Snapshot)) -> Snapshot {
    let snap = {
        let shared = app.state::<Shared>();
        let mut g = shared.snapshot.lock().unwrap();
        f(&mut g);
        g.authorized = auth::is_authorized(&shared.dirs);
        g.account = auth::account(&shared.dirs);
        g.suppressed = shared.lock.lock().unwrap().suppressed_date.as_deref() == Some(today_str().as_str());
        g.rpg = rpg::view(&shared.rpg.lock().unwrap());
        g.timer = shared.timer.lock().unwrap().clone();
        {
            let p = shared.plan.lock().unwrap();
            g.sessions_today = p.sessions_today;
            g.focus_secs_today = p.focus_secs_today;
        }
        g.next_holiday = plan::first_holiday_after(&shared.holidays.lock().unwrap().dates, &today_str());
        g.clone()
    };
    let _ = app.emit("gate-status", &snap);
    snap
}

pub fn report_error(app: &AppHandle, e: &anyhow::Error) {
    let msg = format!("{e:#}");
    publish(app, move |s| s.error = Some(msg));
}

/// Level-up / rank-up notifications.
fn announce_progress(app: &AppHandle, before: u64, after: u64) {
    let (b, a) = (rpg::level_info(before).level, rpg::level_info(after).level);
    if a > b {
        let (rb, _, _) = rpg::rank_for(b);
        let (ra, _, title) = rpg::rank_for(a);
        if ra != rb {
            notify::send(app, "Rank up!", &format!("You reached {ra}-Rank: {title}."));
        } else {
            notify::send(app, "Level up!", &format!("You are now level {a}."));
        }
    }
}

/// Pays out EXP for completed tasks that have not paid yet.
fn award(app: &AppHandle, list: &[Task]) {
    if list.is_empty() {
        return;
    }
    let (before, after) = {
        let shared = app.state::<Shared>();
        let mut r = shared.rpg.lock().unwrap();
        let before = r.total_exp();
        let gained: u64 = list.iter().map(|t| r.award(t)).sum();
        if gained > 0 {
            let _ = save_json(&shared.dirs.rpg(), &*r);
        }
        (before, r.total_exp())
    };
    announce_progress(app, before, after);
}

/// Where an unresolved quest goes: your next holiday, or tomorrow if none is marked.
fn shift_target(holidays: &[String]) -> (String, bool) {
    match plan::first_holiday_after(holidays, &today_str()) {
        Some(d) => (d, true),
        None => (tomorrow_str(), false),
    }
}

fn days_between(from: &str, to: &str) -> i64 {
    let p = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    match (p(from), p(to)) {
        (Some(a), Some(b)) => (b - a).num_days(),
        _ => i64::MAX,
    }
}

/// Reads the holiday list at most once an hour (10 minutes after a failure).
async fn refresh_holidays(app: &AppHandle, token: &str) {
    let now = now_secs();
    {
        let shared = app.state::<Shared>();
        let mut h = shared.holidays.lock().unwrap();
        if now - h.tried_at < 3600 {
            return;
        }
        h.tried_at = now;
    }
    let from = Local::now().date_naive().succ_opt().unwrap_or_else(|| Local::now().date_naive());
    let result = calendar::holidays(token, from, HOLIDAY_LOOKAHEAD_DAYS).await;
    let shared = app.state::<Shared>();
    let mut h = shared.holidays.lock().unwrap();
    match result {
        Ok(dates) => h.dates = dates,
        Err(_) => h.tried_at = now - 3000, // retry in ~10 minutes; keep the old list
    }
}

/// Fetches Tasks and Calendar with one shared access token, awards EXP, builds the
/// day plan and returns the Gate decision. If either API fails nothing changes.
async fn refresh_all(app: &AppHandle) -> Result<Decision> {
    let (dirs, cfg) = {
        let shared = app.state::<Shared>();
        let cfg = shared.config.lock().unwrap().clone();
        (shared.dirs.clone(), cfg)
    };
    let token = match auth::access_token(&dirs, &cfg).await {
        Ok(t) => t,
        Err(e) => {
            report_error(app, &e);
            return Err(e);
        }
    };

    let (tasks_res, cal_res) = tokio::join!(tasks::fetch(&token, &cfg.task_list_id), calendar::fetch(&token));
    if let Ok(f) = &tasks_res {
        award(app, &f.done_today);
    }
    let (f, cal) = match (tasks_res, cal_res) {
        (Ok(f), Ok(c)) => (f, c),
        (t, c) => {
            let mut problems = Vec::new();
            if let Err(e) = t {
                problems.push(format!("Google Tasks: {e:#}"));
            }
            if let Err(e) = c {
                problems.push(format!("Google Calendar: {e:#}"));
            }
            let err = anyhow!(problems.join("\n"));
            report_error(app, &err);
            return Err(err);
        }
    };
    refresh_holidays(app, &token).await;
    Ok(evaluate(app, &cfg, f, cal))
}

/// Builds the plan, resolves missed blocks, sends notifications, decides, publishes.
fn evaluate(app: &AppHandle, cfg: &Config, f: tasks::Fetched, cal: calendar::Fetched) -> Decision {
    let shared = app.state::<Shared>();
    let now = now_secs();
    let today = today_str();
    let midnight = local_midnight();
    let lock_from = plan::parse_hhmm(&cfg.unplanned_lock_from);
    let grace = cfg.missed_grace_mins as i64 * 60;
    let holidays = shared.holidays.lock().unwrap().dates.clone();
    let ctx = plan::Ctx { today: &today, now, midnight };

    let mut notes: Vec<(String, String, String)> = Vec::new(); // (once-id, title, body)
    let mut shifted: Vec<(String, String)> = Vec::new(); // (title, date)
    let shifted_to_holiday: bool;

    let (items, moved, done_events) = {
        let mut p = shared.plan.lock().unwrap();
        p.roll_day(&today);
        let mut items = plan::build(&ctx, &f.pending, &f.done_today, &cal.events, &p);
        let mut dirty = false;

        // Remember every missed block (survives restarts and midnight); forget resolved ones.
        for m in plan::missed_records(&items) {
            if !p.missed.contains_key(&m.key) {
                p.missed.insert(m.key.clone(), m);
                dirty = true;
            }
        }
        let before = p.missed.len();
        p.missed.retain(|k, _| !items.iter().any(|i| &i.key == k && i.status != "missed"));
        dirty |= p.missed.len() != before;

        // Unresolved after the grace period -> your next holiday (tomorrow if none is marked).
        let (target, is_holiday) = shift_target(&holidays);
        shifted_to_holiday = is_holiday;
        for m in plan::due_for_auto_shift(&p.missed, now, grace) {
            p.moves.insert(
                m.key.clone(),
                Move { kind: m.kind.clone(), title: m.title.clone(), date: target.clone(), start_min: None, dur_min: m.dur_min, auto: true },
            );
            p.missed.remove(&m.key);
            shifted.push((m.title, target.clone()));
            dirty = true;
        }
        // Undone unplanned tasks left over from earlier days follow the same rule.
        let stale: Vec<(String, String, u32)> = items
            .iter()
            .filter(|i| i.kind == "task" && i.status == "floating" && i.overdue && !i.moved)
            .filter(|i| i.due.as_deref().map_or(false, |d| days_between(d, &today) <= AUTO_SHIFT_MAX_AGE_DAYS))
            .take(AUTO_SHIFT_MAX_PER_PASS)
            .map(|i| (i.key.clone(), i.title.clone(), i.dur_min))
            .collect();
        for (key, title, dur) in stale {
            p.moves.insert(key, Move { kind: "task".into(), title: title.clone(), date: target.clone(), start_min: None, dur_min: dur, auto: true });
            shifted.push((title, target.clone()));
            dirty = true;
        }
        if !shifted.is_empty() {
            items = plan::build(&ctx, &f.pending, &f.done_today, &cal.events, &p);
        }

        // Notifications to send (once each per day) after the lock is released.
        for i in items.iter().filter(|i| !i.rest && !i.done) {
            let (Some(s), Some(_)) = (i.start, i.end) else { continue };
            match i.status {
                "upcoming" if s - now <= SOON_SECS => notes.push((
                    format!("soon:{}:{today}", i.key),
                    "Quest starting soon".into(),
                    format!("{} starts in {} min.", i.title, ((s - now + 59) / 60).max(1)),
                )),
                "active" if now - s <= 900 => notes.push((
                    format!("start:{}:{today}", i.key),
                    "Quest started".into(),
                    format!("{} is in progress. The Gate stays sealed until you finish it.", i.title),
                )),
                "missed" => notes.push((
                    format!("missed:{}:{today}", i.key),
                    "Quest not finished".into(),
                    format!("{} was not finished. Reschedule it within {} min or it moves to your next holiday.", i.title, cfg.missed_grace_mins),
                )),
                _ => {}
            }
        }
        if dirty {
            let _ = save_json(&shared.dirs.plan(), &*p);
        }
        (items, plan::moved_away(&p, &today), p.done_events.len())
    };

    for (id, title, body) in notes {
        notify::once(app, &id, &title, &body);
    }
    if !shifted.is_empty() {
        let (date, n) = (&shifted[0].1, shifted.len());
        let where_to = if shifted_to_holiday { "your next holiday" } else { "tomorrow (no holiday found in your calendar)" };
        notify::send(app, "Quests rescheduled", &format!("{n} unfinished quest{} moved to {where_to} ({date}).", if n == 1 { "" } else { "s" }));
    }

    let decision = plan::decide(&items, now, midnight, lock_from, grace);
    let moved_view: Vec<MovedView> =
        moved.into_iter().map(|(key, m)| MovedView { key, title: m.title, date: m.date, start_min: m.start_min, auto: m.auto }).collect();
    let done_today = f.done_today.len() + done_events;
    let d = decision.clone();
    let note = cal.note;
    publish(app, move |s| {
        s.items = items;
        s.moved = moved_view;
        s.done_today = done_today;
        s.decision = Some(d);
        s.calendar_note = Some(note);
        s.last_checked = Some(now_secs() as u64);
        s.error = None;
    });
    decision
}

async fn wait_for(mut cond: impl FnMut() -> bool, secs: u64) -> bool {
    for _ in 0..(secs * 2) {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    cond()
}

/// Seals the Gate (hosts file + apps) with one admin prompt.
async fn engage(app: &AppHandle) -> Result<()> {
    let shared = app.state::<Shared>();
    let _guard = shared.busy.lock().await;
    if is_locked(app) {
        return Ok(());
    }
    let cfg = shared.config.lock().unwrap().clone();
    if cfg.blocked_domains.is_empty() && cfg.blocked_apps.is_empty() {
        bail!("Nothing to block. Add sites or apps in System Settings.");
    }

    let staging = shared.dirs.staging()?;
    let mut script = Script::new();
    if !cfg.blocked_domains.is_empty() {
        let staged = hosts::stage(&staging, Some(&cfg.blocked_domains))?;
        hosts::push_install(&mut script, &staged);
    }
    let (locks, missing) = apps::plan_lock(&cfg.blocked_apps, &mut script).await;

    privilege::run_elevated(&script, &staging).await?;
    if !cfg.blocked_domains.is_empty() && !wait_for(hosts::has_block, 120).await {
        bail!("The hosts file was not updated. Approve the administrator prompt.");
    }
    privilege::cleanup(&staging);
    apps::kill_running(&locks).await;

    {
        let mut st = shared.lock.lock().unwrap();
        st.locked = true;
        st.apps = locks;
        save_json(&shared.dirs.state(), &*st)?;
    }
    publish(app, move |s| {
        s.locked = true;
        s.error = (!missing.is_empty()).then(|| format!("Could not find: {}", missing.join(", ")));
    });
    Ok(())
}

/// Reverses everything `engage` did.
pub async fn release(app: &AppHandle) -> Result<()> {
    let shared = app.state::<Shared>();
    let _guard = shared.busy.lock().await;

    let (locked, locks) = {
        let st = shared.lock.lock().unwrap();
        (st.locked, st.apps.clone())
    };
    if !locked {
        return Ok(());
    }
    let staging = shared.dirs.staging()?;
    let mut script = Script::new();
    let had_block = hosts::has_block();
    if had_block {
        let staged = hosts::stage(&staging, None)?;
        hosts::push_install(&mut script, &staged);
    }
    apps::plan_unlock(&locks, &mut script);

    privilege::run_elevated(&script, &staging).await?;
    if had_block && !wait_for(|| !hosts::has_block(), 120).await {
        bail!("The hosts file was not restored. Approve the administrator prompt and retry.");
    }
    privilege::cleanup(&staging);

    {
        let mut st = shared.lock.lock().unwrap();
        st.locked = false;
        st.apps.clear();
        save_json(&shared.dirs.state(), &*st)?;
    }
    publish(app, |s| {
        s.locked = false;
        s.error = None;
    });
    Ok(())
}

/// Emergency release: lifts the seal and keeps auto-enforcement off until local midnight.
pub async fn surrender(app: &AppHandle) -> Result<()> {
    {
        let shared = app.state::<Shared>();
        let mut st = shared.lock.lock().unwrap();
        st.suppressed_date = Some(today_str());
        save_json(&shared.dirs.state(), &*st)?;
    }
    publish(app, |_| {});
    release(app).await
}

fn should_engage(app: &AppHandle) -> bool {
    let shared = app.state::<Shared>();
    let has_targets = {
        let c = shared.config.lock().unwrap();
        !c.blocked_domains.is_empty() || !c.blocked_apps.is_empty()
    };
    let suppressed = {
        let st = shared.lock.lock().unwrap();
        st.suppressed_date.as_deref() == Some(today_str().as_str())
    };
    has_targets && !suppressed && now_secs() >= shared.engage_retry_after.load(Ordering::SeqCst)
}

/// One supervisor cycle (also what the "Check now" button runs).
pub async fn tick(app: &AppHandle) -> Result<()> {
    if !auth::is_authorized(&app.state::<Shared>().dirs) {
        publish(app, |_| {}); // nothing to enforce until Google is connected
        return Ok(());
    }
    let decision = refresh_all(app).await?;
    let locked = is_locked(app);
    if locked && !decision.sealed {
        release(app).await?;
        notify::send(app, "Gate open", &decision.reason);
    } else if !locked && decision.sealed && should_engage(app) {
        match engage(app).await {
            Ok(()) => notify::send(app, "Gate sealed", &decision.reason),
            Err(e) => {
                app.state::<Shared>().engage_retry_after.store(now_secs() + ENGAGE_RETRY_SECS, Ordering::SeqCst);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// After a change you made: refresh, and open the Gate at once if nothing is due any more.
async fn settle(app: &AppHandle) -> Result<()> {
    let decision = refresh_all(app).await?;
    if is_locked(app) && !decision.sealed {
        // Don't make the click wait for the admin prompt.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match release(&app).await {
                Ok(()) => notify::send(&app, "Gate open", &decision.reason),
                Err(e) => report_error(&app, &e),
            }
        });
    }
    Ok(())
}

/// Finds a quest in the current plan (not a rest block, not finished).
fn find_item(app: &AppHandle, key: &str) -> Option<Item> {
    app.state::<Shared>().snapshot.lock().unwrap().items.iter().find(|i| i.key == key && !i.rest && !i.done).cloned()
}

/// Checks off a quest in Google Tasks, pays EXP, and releases at once if nothing is due any more.
pub async fn complete_task(app: &AppHandle, task_id: &str) -> Result<()> {
    if !tasks::valid_id(task_id) {
        bail!("Invalid task id");
    }
    let (dirs, cfg) = {
        let shared = app.state::<Shared>();
        let cfg = shared.config.lock().unwrap().clone();
        (shared.dirs.clone(), cfg)
    };
    let token = auth::access_token(&dirs, &cfg).await?;
    let done = tasks::complete(&token, &cfg.task_list_id, task_id).await?;
    award(app, &[done]);
    {
        let shared = app.state::<Shared>();
        let mut p = shared.plan.lock().unwrap();
        p.missed.remove(&format!("task:{task_id}"));
        let _ = save_json(&shared.dirs.plan(), &*p);
    }
    settle(app).await
}

/// Ticks a calendar event in the app (Google Calendar has no "done" state, so this is
/// stored locally and the calendar itself is never modified). Pays EXP by length;
/// less if the focus timer was never used on it.
pub async fn complete_event(app: &AppHandle, key: &str) -> Result<()> {
    let item = find_item(app, key).filter(|i| i.kind == "event").ok_or_else(|| anyhow!("That event is no longer in your plan. Check again."))?;
    let (before, after) = {
        let shared = app.state::<Shared>();
        let tracked = {
            let mut p = shared.plan.lock().unwrap();
            let tracked = p.started.contains_key(key) || p.focus_secs.get(key).copied().unwrap_or(0) > 0;
            if !p.done_events.iter().any(|k| k == key) {
                p.done_events.push(key.to_string());
            }
            p.missed.remove(key);
            let _ = save_json(&shared.dirs.plan(), &*p);
            tracked
        };
        let mut r = shared.rpg.lock().unwrap();
        let before = r.total_exp();
        r.award_event(key, &item.title, item.dur_min as i64, tracked);
        let _ = save_json(&shared.dirs.rpg(), &*r);
        (before, r.total_exp())
    };
    announce_progress(app, before, after);
    settle(app).await
}

/// Moves a quest. `mode`: `"holiday"` (your next calendar holiday) or `"date"` (a day you pick,
/// optionally with a start time; without one the quest is "unplanned" that day).
/// Also brings a previously rescheduled quest back (pick today). Local only.
pub async fn reschedule(app: &AppHandle, key: &str, mode: &str, date: Option<String>, start_min: Option<u32>, dur_min: Option<u32>) -> Result<()> {
    let (kind, title, cur_dur) = match find_item(app, key) {
        Some(i) => (i.kind.to_string(), i.title, i.dur_min),
        None => {
            // not in today's plan: maybe it was moved away earlier and you are bringing it back
            let shared = app.state::<Shared>();
            let p = shared.plan.lock().unwrap();
            match p.moves.get(key) {
                Some(m) => (m.kind.clone(), m.title.clone(), m.dur_min),
                None => bail!("That quest is no longer in your plan. Check again."),
            }
        }
    };
    let today = today_str();
    let (target, start) = match mode {
        "holiday" => {
            let holidays = app.state::<Shared>().holidays.lock().unwrap().dates.clone();
            match plan::first_holiday_after(&holidays, &today) {
                Some(d) => (d, None),
                None => bail!("No holiday found in your calendar for the next {HOLIDAY_LOOKAHEAD_DAYS} days. Pick a date instead."),
            }
        }
        "date" => (date.ok_or_else(|| anyhow!("Pick a date."))?, start_min),
        _ => bail!("Unknown reschedule mode."),
    };
    let dur = dur_min.unwrap_or(cur_dur);
    let now_min = ((now_secs() - local_midnight()) / 60).max(0) as u32;
    plan::validate_move(&target, start, dur, &today, now_min)?;
    {
        let shared = app.state::<Shared>();
        let mut p = shared.plan.lock().unwrap();
        p.moves.insert(key.to_string(), Move { kind, title, date: target, start_min: start, dur_min: dur, auto: false });
        p.missed.remove(key);
        let _ = save_json(&shared.dirs.plan(), &*p);
    }
    settle(app).await
}

/* ---------------- Pomodoro ---------------- */

fn record_focus(app: &AppHandle, key: Option<&str>, secs: u64, finished_session: bool) {
    let shared = app.state::<Shared>();
    let mut p = shared.plan.lock().unwrap();
    p.focus_secs_today += secs;
    if finished_session {
        p.sessions_today += 1;
    }
    if let Some(k) = key {
        *p.focus_secs.entry(k.to_string()).or_insert(0) += secs;
    }
    let _ = save_json(&shared.dirs.plan(), &*p);
}

fn spawn_phase(app: &AppHandle, generation: u64, ends_at: i64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let wait = (ends_at - now_secs()).max(0) as u64;
        tokio::time::sleep(Duration::from_secs(wait)).await;
        advance_timer(&app, generation);
    });
}

fn begin_phase(app: &AppHandle, phase: Phase, key: Option<String>, title: Option<String>, cycle: u32) {
    let cfg = app.state::<Shared>().config.lock().unwrap().clone();
    let mins = timer::minutes_for(phase, cfg.focus_min, cfg.short_break_min, cfg.long_break_min) as i64;
    let now = now_secs();
    let generation = {
        let shared = app.state::<Shared>();
        let mut t = shared.timer.lock().unwrap();
        let g = t.generation + 1;
        *t = TimerState { phase, quest_key: key, quest_title: title, started_at: now, ends_at: now + mins * 60, cycle, generation: g };
        g
    };
    if phase != Phase::Idle {
        spawn_phase(app, generation, now + mins * 60);
    }
}

/// A timer phase ran out (called by the sleeping task; ignored if superseded).
fn advance_timer(app: &AppHandle, generation: u64) {
    let (phase, key, title, cycle, secs) = {
        let t = app.state::<Shared>().timer.lock().unwrap().clone();
        if t.generation != generation {
            return;
        }
        (t.phase, t.quest_key.clone(), t.quest_title.clone(), t.cycle, (t.ends_at - t.started_at).max(0) as u64)
    };
    let cfg = app.state::<Shared>().config.lock().unwrap().clone();
    match phase {
        Phase::Focus => {
            record_focus(app, key.as_deref(), secs, true);
            let next = timer::after_focus(cycle + 1, cfg.long_break_every);
            let mins = timer::minutes_for(next, cfg.focus_min, cfg.short_break_min, cfg.long_break_min);
            begin_phase(app, next, key, title, cycle + 1);
            notify::send(app, "Focus session complete", &format!("Take a {mins} minute break. Well done."));
        }
        Phase::ShortBreak | Phase::LongBreak => {
            begin_phase(app, Phase::Idle, None, None, cycle);
            notify::send(app, "Break over", "Ready for the next focus session?");
        }
        Phase::Idle => {}
    }
    publish(app, |_| {});
}

/// Starts a focus session, optionally on a quest (which marks it "in progress").
pub fn timer_start(app: &AppHandle, key: Option<String>) -> Result<()> {
    let title = match &key {
        Some(k) => Some(find_item(app, k).ok_or_else(|| anyhow!("That quest is no longer in your plan."))?.title),
        None => None,
    };
    let (running_focus, cycle) = {
        let shared = app.state::<Shared>();
        let t = shared.timer.lock().unwrap();
        (t.phase == Phase::Focus, t.cycle)
    };
    if running_focus {
        timer_stop(app); // switching quests: bank the time spent so far
    }
    if let Some(k) = &key {
        let shared = app.state::<Shared>();
        let mut p = shared.plan.lock().unwrap();
        p.started.entry(k.clone()).or_insert_with(now_secs);
        let _ = save_json(&shared.dirs.plan(), &*p);
    }
    begin_phase(app, Phase::Focus, key, title, cycle);
    publish(app, |s| {
        // the quest now shows "in progress" without waiting for the next poll
        let started: Vec<String> = s.timer.quest_key.iter().cloned().collect();
        for i in s.items.iter_mut().filter(|i| started.contains(&i.key) && !i.done) {
            i.in_progress = true;
        }
    });
    Ok(())
}

/// Stops the timer. Focus time spent so far (a minute or more) is kept.
pub fn timer_stop(app: &AppHandle) {
    let (phase, key, started, cycle) = {
        let t = app.state::<Shared>().timer.lock().unwrap().clone();
        (t.phase, t.quest_key, t.started_at, t.cycle)
    };
    if phase == Phase::Focus {
        let spent = (now_secs() - started).max(0) as u64;
        if spent >= 60 {
            record_focus(app, key.as_deref(), spent, false);
        }
    }
    begin_phase(app, Phase::Idle, None, None, cycle);
    publish(app, |_| {});
}

/// Normal cadence, but wake exactly when something changes: a block starting soon, starting, ending,
/// or a missed block's grace period running out.
fn next_wait(app: &AppHandle) -> Duration {
    let shared = app.state::<Shared>();
    let cfg = shared.config.lock().unwrap().clone();
    let interval = cfg.poll_interval_secs.max(15) as i64;
    let grace = cfg.missed_grace_mins as i64 * 60;
    let now = now_secs();
    let snap = shared.snapshot.lock().unwrap().clone();
    let mut wake = i64::MAX;
    for i in snap.items.iter().filter(|i| !i.rest && !i.done) {
        if let (Some(s), Some(e)) = (i.start, i.end) {
            for t in [s - SOON_SECS, s, e, e + grace] {
                if t > now {
                    wake = wake.min(t);
                }
            }
        }
    }
    if let Some(u) = snap.decision.and_then(|d| d.until) {
        if u > now {
            wake = wake.min(u);
        }
    }
    let secs = if wake == i64::MAX { interval } else { interval.min((wake - now + 1).max(2)) };
    Duration::from_secs(secs as u64)
}

/// Starts the never-ending enforcement loop (at most one).
pub fn start_supervisor(app: &AppHandle) {
    if app.state::<Shared>().supervisor_running.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let wait = match tick(&app).await {
                Ok(()) => next_wait(&app),
                Err(e) => {
                    report_error(&app, &e);
                    Duration::from_secs(30) // offline at login? retry soon
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
}

/// Snapshot with the live flags filled in.
pub fn snapshot(app: &AppHandle) -> Snapshot {
    let shared = app.state::<Shared>();
    let mut s = shared.snapshot.lock().unwrap().clone();
    s.authorized = auth::is_authorized(&shared.dirs);
    s.account = auth::account(&shared.dirs);
    s.timer = shared.timer.lock().unwrap().clone();
    s
}
