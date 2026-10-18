//! Desktop notifications. Sent from Rust so they work while the window is hidden.

use crate::gate::Shared;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// Shows a notification (if enabled in settings). Failures are ignored: a missing
/// notification daemon must never affect the Gate.
pub fn send(app: &AppHandle, title: &str, body: &str) {
    let on = app.state::<Shared>().config.lock().unwrap().notifications;
    if !on {
        return;
    }
    let _ = app.notification().builder().title(title).body(body).show();
}

/// Like [`send`], but each `id` fires only once per day (blocks are re-evaluated every poll).
pub fn once(app: &AppHandle, id: &str, title: &str, body: &str) {
    let first = {
        let shared = app.state::<Shared>();
        let mut p = shared.plan.lock().unwrap();
        let first = p.first_time(id);
        if first {
            let _ = crate::config::save_json(&shared.dirs.plan(), &*p);
        }
        first
    };
    if first {
        send(app, title, body);
    }
}
