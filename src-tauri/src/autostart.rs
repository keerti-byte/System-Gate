//! Install a login autostart entry so the Gate engages as soon as the OS session starts.

use anyhow::Result;
use std::{env, fs, path::PathBuf};

/// Writes (or refreshes) a user-level autostart entry pointing at this binary.
pub fn install() -> Result<()> {
    let exe = env::current_exe()?;
    let exe = exe.display().to_string();
    #[cfg(target_os = "linux")]
    {
        let dir = autostart_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join("system-gate.desktop");
        let body = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=System Gate\n\
             Comment=Engage the Penalty Zone at login\n\
             Exec={exe}\n\
             Icon=applications-system\n\
             Terminal=false\n\
             Categories=Utility;\n\
             X-GNOME-Autostart-enabled=true\n\
             X-GNOME-Autostart-Delay=2\n"
        );
        fs::write(path, body)?;
    }
    #[cfg(target_os = "macos")]
    {
        let home = env::var("HOME")?;
        let dir = PathBuf::from(home).join("Library/LaunchAgents");
        fs::create_dir_all(&dir)?;
        let path = dir.join("dev.systemgate.app.plist");
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.systemgate.app</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><true/>
</dict></plist>
"#
        );
        fs::write(path, body)?;
    }
    #[cfg(target_os = "windows")]
    {
        let startup = windows_startup_dir()?;
        fs::create_dir_all(&startup)?;
        let link = startup.join("System Gate.bat");
        fs::write(link, format!("start \"\" \"{exe}\"\r\n"))?;
    }
    let _ = exe;
    Ok(())
}

#[cfg(target_os = "linux")]
fn autostart_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("autostart");
    }
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".config/autostart")
}

#[cfg(target_os = "windows")]
fn windows_startup_dir() -> Result<PathBuf> {
    let appdata = env::var("APPDATA")?;
    Ok(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
}
