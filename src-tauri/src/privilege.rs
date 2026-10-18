//! OS detection and privilege elevation.
//!
//! Design: the app itself never runs as root/Administrator. Everything that
//! needs elevated rights is collected into a single [`Script`] and executed
//! once through the platform's native prompt:
//!
//! * Linux   — `pkexec` (graphical polkit prompt); falls back to `sudo` in a terminal emulator
//! * macOS   — `osascript … with administrator privileges`
//! * Windows — PowerShell `Start-Process -Verb RunAs` (UAC prompt)
//!
//! One prompt per lock and one per unlock, instead of one per command.

use anyhow::{anyhow, bail, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

pub fn current_os() -> Os {
    match std::env::consts::OS {
        "windows" => Os::Windows,
        "macos" => Os::MacOs,
        _ => Os::Linux, // linux, freebsd, … all behave like Linux here
    }
}

/// Wraps `s` in single quotes for POSIX `sh`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A list of shell (`sh`) or batch (`cmd`) lines to run with elevated rights.
pub struct Script {
    os: Os,
    lines: Vec<String>,
}

impl Script {
    pub fn new() -> Self {
        Self { os: current_os(), lines: Vec::new() }
    }
    pub fn os(&self) -> Os {
        self.os
    }
    /// A line that must succeed (Unix: the script aborts via `set -e`).
    pub fn raw(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
    /// A best-effort line: failure is ignored so later steps still run.
    pub fn soft(&mut self, line: impl Into<String>) {
        let l: String = line.into();
        self.lines.push(if self.os == Os::Windows { l } else { format!("{l} || true") });
    }
    /// Copy `src` to `dst` only if `dst` does not exist yet (first-run backup).
    pub fn backup_once(&mut self, src: &Path, dst: &str) {
        if self.os == Os::Windows {
            self.raw(format!(r#"if not exist "{dst}" copy "{}" "{dst}" >nul"#, src.display()));
        } else {
            self.raw(format!(
                "[ -f {d} ] || cp {s} {d}",
                d = sh_quote(dst),
                s = sh_quote(&src.to_string_lossy())
            ));
        }
    }
    /// Overwrite `dst` with `src`. `cp`/`copy` keep the destination's owner and mode.
    pub fn copy_over(&mut self, src: &Path, dst: &Path) {
        if self.os == Os::Windows {
            self.raw(format!(r#"copy /Y "{}" "{}" >nul"#, src.display(), dst.display()));
        } else {
            self.raw(format!(
                "cp -f {} {}",
                sh_quote(&src.to_string_lossy()),
                sh_quote(&dst.to_string_lossy())
            ));
        }
    }
    /// Make the OS (and its resolver cache) notice the new hosts file.
    pub fn flush_dns(&mut self) {
        match self.os {
            Os::Windows => self.raw("ipconfig /flushdns >nul"),
            Os::MacOs => {
                self.soft("dscacheutil -flushcache");
                self.soft("killall -HUP mDNSResponder");
            }
            Os::Linux => {
                self.soft("resolvectl flush-caches");
                self.soft("nscd -i hosts");
            }
        }
    }
    fn render(&self) -> String {
        match self.os {
            Os::Windows => format!("@echo off\r\n{}\r\n", self.lines.join("\r\n")),
            _ => format!("#!/bin/sh\nset -e\n{}\n", self.lines.join("\n")),
        }
    }
}

/// Runs the script with admin rights. Blocks until the prompt is resolved.
///
/// The script file is intentionally *not* deleted here: some terminal
/// emulators return before `sudo` has read it. Call [`cleanup`] after verifying.
pub async fn run_elevated(script: &Script, staging: &Path) -> Result<()> {
    if script.lines.is_empty() {
        return Ok(());
    }
    let ext = if script.os == Os::Windows { "bat" } else { "sh" };
    let path = staging.join(format!("apply.{ext}"));
    fs::write(&path, script.render())?;
    crate::config::restrict(&path, 0o700)?;
    match script.os {
        Os::Linux => run_linux(&path).await,
        Os::MacOs => run_macos(&path).await,
        Os::Windows => run_windows(&path).await,
    }
}

pub fn cleanup(staging: &Path) {
    for f in ["apply.sh", "apply.bat", "hosts.new"] {
        let _ = fs::remove_file(staging.join(f));
    }
}

async fn run_linux(path: &Path) -> Result<()> {
    if which("pkexec").is_some() {
        let status = Command::new("pkexec").arg("/bin/sh").arg(path).status().await?;
        if status.success() {
            return Ok(());
        }
        bail!("Administrator prompt was cancelled or failed ({status})");
    }
    // Fallback: prompt for the sudo password inside a terminal window.
    let terminals: [(&str, &[&str]); 4] = [
        ("x-terminal-emulator", &["-e"]),
        ("gnome-terminal", &["--wait", "--"]),
        ("konsole", &["-e"]),
        ("xterm", &["-e"]),
    ];
    for (term, flags) in terminals {
        if which(term).is_some() {
            Command::new(term).args(flags).arg("sudo").arg("/bin/sh").arg(path).status().await?;
            // Exit codes are unreliable across terminals; the caller verifies the result.
            return Ok(());
        }
    }
    bail!("Neither pkexec nor a terminal emulator was found. Install polkit (pkexec) or xterm.")
}

async fn run_macos(path: &Path) -> Result<()> {
    let cmd = format!("/bin/sh {}", sh_quote(&path.to_string_lossy()));
    let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
    let apple = format!("do shell script \"{escaped}\" with administrator privileges");
    let out = Command::new("osascript").arg("-e").arg(apple).output().await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "Administrator prompt was cancelled or failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

async fn run_windows(path: &Path) -> Result<()> {
    let ps = format!(
        "Start-Process -FilePath '{}' -Verb RunAs -Wait -WindowStyle Hidden",
        path.display().to_string().replace('\'', "''")
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &ps]);
    quiet(&mut cmd);
    let status = cmd.status().await?;
    if status.success() {
        Ok(())
    } else {
        bail!("UAC prompt was declined or failed ({status})")
    }
}

/// Run a short, non-elevated command and report whether it succeeded.
pub async fn run_quiet(program: &str, args: &[&str]) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(args);
    quiet(&mut cmd);
    cmd.status().await.map(|s| s.success()).unwrap_or(false)
}

/// Prevents a console window from flashing on Windows.
pub fn quiet(cmd: &mut Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// Minimal cross-platform `which` (Unix semantics; Windows uses `where`).
pub fn which(bin: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file())
}
