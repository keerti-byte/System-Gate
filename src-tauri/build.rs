fn main() {
    // The Google credentials are compiled into the app (see SETUP.md). Re-build when they change.
    println!("cargo:rerun-if-env-changed=SYSTEM_GATE_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=SYSTEM_GATE_GOOGLE_CLIENT_SECRET");
    let id = std::env::var("SYSTEM_GATE_GOOGLE_CLIENT_ID").unwrap_or_default();
    if id.trim().is_empty() || id.contains("PASTE") {
        println!(
            "cargo:warning=SYSTEM_GATE_GOOGLE_CLIENT_ID is not set: this build cannot sign in to Google. See SETUP.md (copy .cargo/config.toml.example to .cargo/config.toml)."
        );
    }
    tauri_build::build()
}
