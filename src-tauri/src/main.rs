//! System Gate: entry point.
//!
//! Module map (each file has one job, so contributors can find things fast):
//!
//! | module       | responsibility                                                    |
//! |--------------|-------------------------------------------------------------------|
//! | `config`     | persisted settings, lock state, input validation, app data paths  |
//! | `privilege`  | OS detection + running ONE batched script with admin rights       |
//! | `hosts`      | pure hosts-file block rendering + staging via `std::fs`           |
//! | `apps`       | browser/app blocking (chmod on Linux/macOS, netsh on Windows)     |
//! | `auth`       | Google OAuth2 (PKCE, loopback redirect, token refresh)            |
//! | `tasks`      | Google Tasks client (pending quests, complete, completed-today)   |
//! | `calendar`   | Google Calendar client (today's timed events)                     |
//! | `plan`       | day plan + the Gate rule (free time / breaks / early finish), tested |
//! | `timer`      | Pomodoro phases; `notify`: desktop notifications                   |
//! | `rpg`        | keyword attributes, EXP, leveling curve, ranks (unit-tested)      |
//! | `gate`       | supervisor loop: auto-engage, auto-release, EXP, UI snapshots     |
//! | `commands`   | thin `#[tauri::command]` IPC layer called by the frontend         |
//!
//! Lifecycle: the app registers itself to start at OS login (release builds),
//! starts hidden with `--background`, lives in the system tray, and enforces
//! continuously. Closing the window only hides it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod auth;
mod calendar;
mod commands;
mod config;
mod gate;
mod hosts;
mod notify;
mod plan;
mod timer;
mod privilege;
mod rpg;
mod tasks;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open System", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Must be first: a second launch (autostart + manual) just shows the window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--background"])))
        .setup(|app| {
            let root = app.path().app_config_dir()?;
            let shared = gate::Shared::load(config::Dirs::new(root))?;
            let authorized = auth::is_authorized(&shared.dirs);
            app.manage(shared);

            // Hidden when launched at login, unless first-run setup is still needed.
            let background = std::env::args().any(|a| a == "--background");
            if !background || !authorized {
                show_main(app.handle());
            }
            build_tray(app)?;

            // Start at OS login. Skipped in dev so `tauri dev` doesn't register a debug binary.
            if !cfg!(debug_assertions) {
                let _ = app.autolaunch().enable();
            }
            gate::start_supervisor(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::get_status,
            commands::refresh_status,
            commands::complete_task,
            commands::complete_event,
            commands::reschedule,
            commands::timer_start,
            commands::timer_stop,
            commands::google_login,
            commands::google_logout,
            commands::emergency_release,
        ])
        .run(tauri::generate_context!())
        .expect("error while running System Gate");
}
