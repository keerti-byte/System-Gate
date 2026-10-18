//! Blocking specific browsers/apps.
//!
//! * Windows      — outbound + inbound firewall rules via `netsh advfirewall`
//!                  (the app still opens, but has no network access).
//! * Linux/macOS  — `chmod a-x` on the executable (it cannot be launched);
//!                  original permission bits are saved and restored on unlock.
//!
//! Matching processes that are already running are terminated on lock.
//! Limitation: sandboxed installs (Snap/Flatpak/read-only volumes) can ignore
//! chmod — see the README.

use crate::{
    config::AppLock,
    privilege::{current_os, quiet, run_quiet, sh_quote, which, Os, Script},
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

/// Finds the executable for a user-supplied name or path.
pub async fn resolve(spec: &str) -> Option<PathBuf> {
    match current_os() {
        Os::Windows => resolve_windows(spec).await,
        Os::MacOs => resolve_macos(spec),
        Os::Linux => resolve_linux(spec),
    }
}

fn resolve_linux(spec: &str) -> Option<PathBuf> {
    let p = Path::new(spec);
    let found = if p.is_absolute() {
        p.is_file().then(|| p.to_path_buf())
    } else {
        which(spec).or_else(|| which(&spec.to_lowercase()))
    };
    // Follow symlinks: chmod must hit the real binary, not the link.
    found.and_then(|f| fs::canonicalize(f).ok())
}

fn resolve_macos(spec: &str) -> Option<PathBuf> {
    let p = Path::new(spec);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }
    let bundle = if spec.ends_with(".app") {
        PathBuf::from(spec)
    } else {
        PathBuf::from(format!("/Applications/{spec}.app"))
    };
    if let Ok(rd) = fs::read_dir(bundle.join("Contents/MacOS")) {
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        if let Some(first) = files.into_iter().next() {
            return Some(first);
        }
    }
    which(spec)
}

async fn resolve_windows(spec: &str) -> Option<PathBuf> {
    let p = Path::new(spec);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exe = if spec.to_lowercase().ends_with(".exe") { spec.to_string() } else { format!("{spec}.exe") };
    let stem = exe.trim_end_matches(".exe").to_string();

    let mut cmd = Command::new("where");
    cmd.arg(&exe);
    quiet(&mut cmd);
    if let Ok(out) = cmd.output().await {
        if out.status.success() {
            if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().next() {
                let c = PathBuf::from(line.trim());
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(root) = std::env::var_os(var).map(PathBuf::from) {
            for c in [
                root.join(&stem).join(&exe),
                root.join(&stem).join("Application").join(&exe),
                root.join("Programs").join(&stem).join(&exe),
                root.join("Programs").join(&stem).join("Application").join(&exe),
            ] {
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn rule_name(spec: &str) -> String {
    let clean: String = Path::new(spec)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.to_string())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("SystemGate-{clean}")
}

#[cfg(unix)]
fn unix_mode(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o7777)
}
#[cfg(not(unix))]
fn unix_mode(_p: &Path) -> Option<u32> {
    None
}

/// Appends lock commands to `script`. Returns what was locked and which
/// entries could not be found on this machine.
pub async fn plan_lock(specs: &[String], script: &mut Script) -> (Vec<AppLock>, Vec<String>) {
    let os = script.os();
    let (mut locks, mut missing) = (Vec::new(), Vec::new());
    for spec in specs {
        let Some(path) = resolve(spec).await else {
            missing.push(spec.clone());
            continue;
        };
        let ps = path.to_string_lossy().into_owned();
        if os == Os::Windows {
            let rule = rule_name(spec);
            script.raw(format!(r#"netsh advfirewall firewall delete rule name="{rule}" >nul 2>&1"#));
            for dir in ["out", "in"] {
                script.raw(format!(
                    r#"netsh advfirewall firewall add rule name="{rule}" dir={dir} action=block program="{ps}" enable=yes profile=any >nul"#
                ));
            }
            locks.push(AppLock { name: spec.clone(), path: ps, original_mode: None });
        } else {
            let mode = unix_mode(&path);
            script.soft(format!("chmod a-x {}", sh_quote(&ps)));
            locks.push(AppLock { name: spec.clone(), path: ps, original_mode: mode });
        }
    }
    (locks, missing)
}

/// Appends the commands that undo [`plan_lock`].
pub fn plan_unlock(locks: &[AppLock], script: &mut Script) {
    for l in locks {
        if script.os() == Os::Windows {
            script.raw(format!(r#"netsh advfirewall firewall delete rule name="{}" >nul 2>&1"#, rule_name(&l.name)));
        } else {
            script.soft(format!("chmod {:o} {}", l.original_mode.unwrap_or(0o755), sh_quote(&l.path)));
        }
    }
}

/// Closes already-running instances (no elevation needed for the user's own processes).
pub async fn kill_running(locks: &[AppLock]) {
    for l in locks {
        let Some(base) = Path::new(&l.path).file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if current_os() == Os::Windows {
            run_quiet("taskkill", &["/F", "/IM", &base]).await;
        } else {
            run_quiet("pkill", &["-x", &base]).await;
            run_quiet("pkill", &["-f", "--", &l.path]).await;
        }
    }
}
