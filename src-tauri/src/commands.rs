//! IPC layer: the only functions the frontend can call.
//! Keep these thin; real logic lives in `gate`, `auth`, `config`.

use crate::{
    auth,
    config::{save_json, Config},
    gate::{self, Shared, Snapshot},
};
use tauri::{AppHandle, State};

/// Phrase the user must type to break the seal early.
const SURRENDER_PHRASE: &str = "I SURRENDER";

fn ui<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn get_config(state: State<'_, Shared>) -> Config {
    state.config.lock().unwrap().clone()
}

/// Receives the blocked list (and other settings) from the frontend.
#[tauri::command]
pub fn save_config(state: State<'_, Shared>, mut config: Config) -> Result<Config, String> {
    if state.lock.lock().unwrap().locked {
        return Err("Settings are sealed while the Gate is active.".into());
    }
    ui(config.sanitize())?;
    ui(save_json(&state.dirs.config(), &config))?;
    *state.config.lock().unwrap() = config.clone();
    Ok(config)
}

#[tauri::command]
pub fn get_status(app: AppHandle) -> Snapshot {
    gate::snapshot(&app)
}

/// Runs one enforcement cycle right now.
#[tauri::command]
pub async fn refresh_status(app: AppHandle) -> Result<Snapshot, String> {
    if let Err(e) = gate::tick(&app).await {
        gate::report_error(&app, &e);
    }
    Ok(gate::snapshot(&app))
}

/// Checks a quest off in Google Tasks (PATCH status=completed).
#[tauri::command]
pub async fn complete_task(app: AppHandle, task_id: String) -> Result<Snapshot, String> {
    ui(gate::complete_task(&app, &task_id).await)?;
    Ok(gate::snapshot(&app))
}

/// Ticks a calendar event in the app (local; pays EXP by length).
#[tauri::command]
pub async fn complete_event(app: AppHandle, key: String) -> Result<Snapshot, String> {
    ui(gate::complete_event(&app, &key).await)?;
    Ok(gate::snapshot(&app))
}

/// Moves a quest: `mode` = "holiday" | "date" (with optional start minute and length).
#[tauri::command]
pub async fn reschedule(
    app: AppHandle,
    key: String,
    mode: String,
    date: Option<String>,
    start_min: Option<u32>,
    dur_min: Option<u32>,
) -> Result<Snapshot, String> {
    ui(gate::reschedule(&app, &key, &mode, date, start_min, dur_min).await)?;
    Ok(gate::snapshot(&app))
}

/// Starts a Pomodoro focus session, optionally on a quest.
#[tauri::command]
pub fn timer_start(app: AppHandle, key: Option<String>) -> Result<Snapshot, String> {
    ui(gate::timer_start(&app, key))?;
    Ok(gate::snapshot(&app))
}

#[tauri::command]
pub fn timer_stop(app: AppHandle) -> Snapshot {
    gate::timer_stop(&app);
    gate::snapshot(&app)
}

#[tauri::command]
pub async fn google_login(app: AppHandle, state: State<'_, Shared>) -> Result<(), String> {
    let cfg = state.config.lock().unwrap().clone();
    ui(auth::login(&app, &state.dirs, &cfg).await)
}

#[tauri::command]
pub fn google_logout(state: State<'_, Shared>) -> Result<(), String> {
    if state.lock.lock().unwrap().locked {
        return Err("Cannot disconnect Google while the Gate is active.".into());
    }
    auth::logout(&state.dirs);
    Ok(())
}

/// Escape hatch so a bug can never lock you out of your own machine.
#[tauri::command]
pub async fn emergency_release(app: AppHandle, phrase: String) -> Result<(), String> {
    if phrase.trim() != SURRENDER_PHRASE {
        return Err(format!("Type exactly “{SURRENDER_PHRASE}” to confirm."));
    }
    ui(gate::surrender(&app).await)
}
