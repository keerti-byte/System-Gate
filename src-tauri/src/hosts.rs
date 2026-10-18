//! Hosts-file handling.
//!
//! * Rendering is pure (string in, string out) and unit-tested.
//! * Reading/staging uses `std::fs` in the unprivileged process.
//! * Only the final `copy` into place runs elevated (see `privilege.rs`).
//!
//! Gate entries live between two marker lines. Everything outside the markers
//! is never touched, and a one-time backup is kept next to the hosts file.

use crate::privilege::Script;
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const BEGIN: &str = "# >>> SYSTEM-GATE BEGIN (managed block - do not edit) >>>";
pub const END: &str = "# <<< SYSTEM-GATE END <<<";

pub fn hosts_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root).join(r"System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

fn eol_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Removes the managed block. Inside a block only loopback lines are dropped,
/// so a damaged block (missing END marker) can never swallow unrelated lines.
pub fn strip_block(original: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in original.lines() {
        let t = line.trim();
        if t == BEGIN {
            inside = true;
            continue;
        }
        if inside {
            if t == END {
                inside = false;
                continue;
            }
            if t.starts_with("127.0.0.1") || t.starts_with("::1") {
                continue;
            }
            inside = false; // block was damaged; resume normal processing
        }
        kept.push(line);
    }
    let eol = eol_of(original);
    let mut s = kept.join(eol);
    if !s.is_empty() {
        s.push_str(eol);
    }
    s
}

/// Returns `original` with a fresh managed block pointing `domains` (and their
/// `www.` variants) to loopback over IPv4 and IPv6.
pub fn with_block(original: &str, domains: &[String]) -> String {
    let eol = eol_of(original);
    let mut s = strip_block(original);
    s.push_str(BEGIN);
    s.push_str(eol);
    for d in domains {
        for host in [d.clone(), format!("www.{d}")] {
            s.push_str(&format!("127.0.0.1 {host}{eol}"));
            s.push_str(&format!("::1 {host}{eol}"));
        }
    }
    s.push_str(END);
    s.push_str(eol);
    s
}

pub fn has_block() -> bool {
    fs::read_to_string(hosts_path()).map(|s| s.contains(BEGIN)).unwrap_or(false)
}

/// Writes the desired hosts content to `<staging>/hosts.new`.
/// `Some(domains)` = install block, `None` = remove block.
pub fn stage(staging: &Path, domains: Option<&[String]>) -> Result<PathBuf> {
    let current = fs::read_to_string(hosts_path()).context("reading the system hosts file")?;
    let next = match domains {
        Some(d) => with_block(&current, d),
        None => strip_block(&current),
    };
    let out = staging.join("hosts.new");
    fs::write(&out, next)?;
    Ok(out)
}

/// Adds the "back up once, copy into place, flush DNS" steps to the elevated script.
pub fn push_install(script: &mut Script, staged: &Path) {
    let hp = hosts_path();
    script.backup_once(&hp, &format!("{}.system-gate.bak", hp.display()));
    script.copy_over(staged, &hp);
    script.flush_dns();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "127.0.0.1 localhost\n::1 localhost\n10.0.0.5 nas\n";

    #[test]
    fn block_round_trips() {
        let blocked = with_block(SAMPLE, &["youtube.com".into(), "x.com".into()]);
        assert!(blocked.contains("127.0.0.1 www.youtube.com"));
        assert!(blocked.contains("::1 x.com"));
        assert_eq!(strip_block(&blocked), SAMPLE);
    }

    #[test]
    fn applying_twice_does_not_duplicate() {
        let once = with_block(SAMPLE, &["x.com".into()]);
        let twice = with_block(&once, &["x.com".into()]);
        assert_eq!(once, twice);
    }

    #[test]
    fn damaged_block_never_eats_user_lines() {
        let broken = format!("{SAMPLE}{BEGIN}\n127.0.0.1 x.com\n10.9.9.9 keep-me\n");
        let cleaned = strip_block(&broken);
        assert!(cleaned.contains("keep-me"));
        assert!(!cleaned.contains("x.com"));
    }

    #[test]
    fn preserves_windows_line_endings() {
        let win = "127.0.0.1 localhost\r\n";
        let blocked = with_block(win, &["x.com".into()]);
        assert!(blocked.contains("\r\n") && !blocked.replace("\r\n", "").contains('\n'));
    }
}
