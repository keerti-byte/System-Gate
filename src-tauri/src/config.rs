//! Persisted configuration, lock state, validation and data-directory layout.

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Files System Gate keeps inside the OS app-config directory.
#[derive(Clone)]
pub struct Dirs {
    root: PathBuf,
}

impl Dirs {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("state.json")
    }
    pub fn rpg(&self) -> PathBuf {
        self.root.join("rpg.json")
    }
    /// Reschedules, ticked events, focus time, missed blocks.
    pub fn plan(&self) -> PathBuf {
        self.root.join("plan.json")
    }
    pub fn tokens(&self) -> PathBuf {
        self.root.join("tokens.json")
    }
    /// Private (0700 on Unix) scratch folder for the staged hosts file and the
    /// elevated script. It must not be world-writable: a root-run script that
    /// another user could modify would be a privilege-escalation hole.
    pub fn staging(&self) -> Result<PathBuf> {
        let p = self.root.join("staging");
        fs::create_dir_all(&p)?;
        restrict(&p, 0o700)?;
        Ok(p)
    }
}

/// User-editable settings (sent to / received from the frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub blocked_domains: Vec<String>,
    /// Executable names ("helium"), app names on macOS ("Helium") or absolute paths.
    pub blocked_apps: Vec<String>,
    /// Google Tasks list id. `@default` is the account's primary list.
    pub task_list_id: String,
    pub poll_interval_secs: u64,
    /// Desktop notifications (block starting, Gate sealed/opened, timer, level-ups).
    pub notifications: bool,
    /// Tasks without a time slot lock the Gate from this local time ("HH:MM"), until done or planned.
    pub unplanned_lock_from: String,
    /// A block that ended unfinished waits this long for you to reschedule it, then moves to your next holiday.
    pub missed_grace_mins: u32,
    /// Pomodoro lengths (minutes) and how many focus sessions before a long break.
    pub focus_min: u32,
    pub short_break_min: u32,
    pub long_break_min: u32,
    pub long_break_every: u32,
    pub google_client_id: String,
    pub google_client_secret: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            blocked_domains: vec!["youtube.com".into(), "x.com".into(), "reddit.com".into()],
            blocked_apps: vec![],
            task_list_id: "@default".into(),
            poll_interval_secs: 60,
            notifications: true,
            unplanned_lock_from: "18:00".into(),
            missed_grace_mins: 30,
            focus_min: 25,
            short_break_min: 5,
            long_break_min: 15,
            long_break_every: 4,
            google_client_id: String::new(),
            google_client_secret: String::new(),
        }
    }
}

impl Config {
    /// Normalises and validates every field. Everything that ends up in the
    /// hosts file or in an elevated shell script passes through here first.
    pub fn sanitize(&mut self) -> Result<()> {
        let mut domains = Vec::new();
        for raw in &self.blocked_domains {
            match normalize_domain(raw) {
                Some(d) if !domains.contains(&d) => domains.push(d),
                Some(_) => {}
                None => bail!("“{raw}” is not a valid domain (example: youtube.com)"),
            }
        }
        self.blocked_domains = domains;

        let mut apps = Vec::new();
        for raw in &self.blocked_apps {
            let a = raw.trim().to_string();
            if !is_safe_app(&a) {
                bail!("“{raw}” contains characters that are not allowed in an app name or path");
            }
            if !apps.contains(&a) {
                apps.push(a);
            }
        }
        self.blocked_apps = apps;

        self.task_list_id = self.task_list_id.trim().to_string();
        if self.task_list_id.is_empty() {
            self.task_list_id = "@default".into();
        }
        if !self
            .task_list_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_@".contains(c))
        {
            bail!("Task list id may only contain letters, numbers, - _ and @");
        }
        self.poll_interval_secs = self.poll_interval_secs.clamp(15, 3600);
        let m = crate::plan::parse_hhmm(&self.unplanned_lock_from);
        self.unplanned_lock_from = format!("{:02}:{:02}", m / 60, m % 60);
        self.missed_grace_mins = self.missed_grace_mins.clamp(1, 240);
        self.focus_min = self.focus_min.clamp(1, 120);
        self.short_break_min = self.short_break_min.clamp(1, 60);
        self.long_break_min = self.long_break_min.clamp(1, 120);
        self.long_break_every = self.long_break_every.clamp(2, 12);
        self.google_client_id = self.google_client_id.trim().to_string();
        self.google_client_secret = self.google_client_secret.trim().to_string();
        Ok(())
    }
}

/// An app that was locked, remembered so it can be restored exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLock {
    pub name: String,
    pub path: String,
    /// Original Unix permission bits (Linux/macOS). `None` on Windows.
    pub original_mode: Option<u32>,
}

/// Persisted so enforcement survives app restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LockState {
    pub locked: bool,
    pub apps: Vec<AppLock>,
    /// Local date (YYYY-MM-DD) on which Emergency release was used. Auto-enforcement
    /// stays off for the rest of that day and resumes the next day.
    pub suppressed_date: Option<String>,
}

/// "https://www.YouTube.com/watch?v=1" -> "youtube.com". Returns `None` if unusable.
pub fn normalize_domain(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_lowercase();
    if let Some(i) = s.find("://") {
        s = s[i + 3..].to_string();
    }
    let s = s.split(['/', '?', '#']).next()?.to_string();
    let s = s.trim_start_matches("www.").to_string();
    if s.is_empty() || s.len() > 253 || !s.contains('.') {
        return None;
    }
    let ok = s.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    ok.then_some(s)
}

/// Rejects anything that could break out of a quoted shell/batch argument.
pub fn is_safe_app(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 260
        && !s
            .chars()
            .any(|c| c.is_control() || "'\"`$;&|<>%^".contains(c))
}

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Atomic write (temp file + rename) so a crash never leaves half a file.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
pub fn restrict(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn restrict(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_normalised() {
        assert_eq!(normalize_domain("https://www.YouTube.com/watch?v=1").as_deref(), Some("youtube.com"));
        assert_eq!(normalize_domain("x.com").as_deref(), Some("x.com"));
        assert_eq!(normalize_domain("localhost"), None);
        assert_eq!(normalize_domain("bad domain.com"), None);
        assert_eq!(normalize_domain("a.com; rm -rf /"), None);
    }

    #[test]
    fn unsafe_app_names_are_rejected() {
        assert!(is_safe_app("helium"));
        assert!(is_safe_app(r"C:\Program Files\Helium\helium.exe"));
        assert!(!is_safe_app("helium'; rm -rf /"));
        assert!(!is_safe_app("a&b"));
    }
}
