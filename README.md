# System Gate

A cross-platform desktop app (Windows · macOS · Linux) that **seals your distracting websites and apps whenever something on your plan is due, and opens them again in free time, in breaks, and as soon as you finish early**. Quests come from Google Tasks and Google Calendar. Leveling, ranks, sounds and neon colors are themed after the Solo Leveling "System".

Built with **Tauri 2** (Rust backend, plain HTML/CSS/JS frontend, no bundler).

---

## How it works

1. **Starts at login.** In release builds the app registers itself with the OS (registry Run key / LaunchAgent / `~/.config/autostart`) and launches hidden (`--background`) into the system tray. Closing the window only hides it.
2. **The Gate is closed by default.** A supervisor loop checks Google every N seconds (default 60). If any quest is pending, or a calendar event is still ahead today, it seals the Gate: hosts-file entries for your sites, plus firewall rules or `chmod` for your apps. There is **no "begin" button**.
3. **You check quests off inside the app.** Each quest has a clickable hexagon; the app sends `PATCH … {"status":"completed"}` to the Google Tasks API.
4. **The Gate opens by itself** when both locks are clear (rules below), and re-seals if a new quest appears later.
5. **Emergency release** (System settings, type `I SURRENDER`) lifts the seal and keeps auto-enforcement off until local midnight.

### Dual-lock rules (`src-tauri/src/unlock.rs`)

| Lock | Satisfied when |
|------|----------------|
| **Tasks** | No *pending* task is **due today or overdue**. Completed tasks, tasks due in the future, tasks with no due date, and untitled tasks are ignored (and never shown). |
| **Calendar** | Now is **at or after the end of the last timed event today** (none = satisfied). |

The Gate opens only when **both** hold. Calendar details: primary calendar, recurring events expanded; all-day, cancelled and declined events ignored. If either API call fails, a sealed Gate stays sealed (fail-closed) and the error is shown.

**Progress bar:** `overall = (quests % + schedule-time %) / 2`. Quests % = completed-today ÷ (completed-today + pending). Schedule-time % = how far *now* is between the first event's start and the last event's end.

### Quest times

The Google Tasks API **cannot read a task's time of day** (Google documents that only the date is exposed). To show `14:00 – 16:00` next to a quest, the app links a task to a calendar event **when their titles match** (case and spacing ignored). Name the task exactly like the event and the time range appears.

---

## RPG system (`src-tauri/src/rpg.rs`)

Completing a quest pays EXP **once** (task ids are remembered, so un-checking and re-checking can't farm). Keywords in the task title/notes choose the attribute:

| Attribute | Bar | Keywords |
|-----------|-----|----------|
| **Intellect** | MP | math, quant, code, coding, study, read |
| **Vitals** | HP | gym, workout, sleep, water, run |
| **Perception** | AGI | draw, sing, language, journal |
| **Strength** | STR | chores, clean, lift, build |

* Words match a stem exactly or with a common suffix (`reading`, `runs`, `studies`) but **not** as loose prefixes: `ready` ≠ `read`, `single` ≠ `sing`.
* Each matched attribute gets **20 EXP** (a task can feed several). No keyword = 10 general EXP (counts toward level only).
* Level comes from total EXP; each attribute bar also has its own level on the same curve.
* Edit `KEYWORDS`, `BASE_EXP`, `GROWTH` and the EXP constants at the top of `rpg.rs`.

### Leveling curve

EXP to go from level *n* to *n+1* is `400 × 1.10^(n-1)`. Estimated time at ~3 quests/day (~60 EXP/day):

| Rank | Reached at | Total EXP | ≈ Time |
|------|-----------|-----------|--------|
| E → D | Level 5 | 1,856 | 1 month |
| D → C | Level 15 | 11,188 | 6 months |
| C → B | Level 30 | 59,450 | 2.7 years |
| B → A | Level 50 | 422,875 | 19 years |
| A → S | Level 80 | 7.4 million | not in this lifetime |

The upper ranks are intentionally out of reach; lower `GROWTH` (try `1.06`) if you want them closer.

### Color evolution

| Levels | Rank | Neon |
|--------|------|------|
| 1–4 | E | `#00a2ff` Blue |
| 5–14 | D | `#00ffcc` Cyan |
| 15–29 | C | `#8a2be2` Purple |
| 30–49 | B | `#ff00ff` Magenta |
| 50–79 | A | `#ff0033` Crimson |
| 80+ | S | `#ffd700` Gold |

When your rank changes, a full-screen **SYSTEM EVOLUTION** sequence plays (scan line, RGB-split glitch title) and the whole UI recolors mid-animation. Red is also used for warnings and the Penalty Zone, so locked state is always shown with words as well as color.

---

## Project layout

```
system-gate/
├── package.json
├── src/                         # Frontend (static)
│   ├── index.html               # Status, Quest info, Schedule, System settings
│   ├── style.css                # System-window theme; colors driven by rank
│   ├── sounds.js                # Synthesized sound effects (WebAudio)
│   ├── fx.js                    # Ambient canvas, spark bursts, tweening (decorative only)
│   └── main.js                  # Rendering, Gate/gauges, evolution animation, IPC calls
└── src-tauri/
    ├── Cargo.toml  build.rs  tauri.conf.json  capabilities/default.json
    └── src/
        ├── main.rs              # Tray, autostart, single-instance, supervisor start
        ├── commands.rs          # IPC surface (thin)
        ├── gate.rs              # Supervisor loop: auto-engage/release, EXP, snapshots
        ├── plan.rs              # Day plan + the Gate rule, moves, holidays (unit-tested)
        ├── timer.rs / notify.rs # Pomodoro phases; desktop notifications
        ├── rpg.rs               # Keywords, EXP, curve, ranks (unit-tested)
        ├── tasks.rs             # Google Tasks: pending, completed-today, complete
        ├── calendar.rs          # Google Calendar: today's timed events
        ├── auth.rs              # OAuth2 (PKCE + loopback), built-in credentials, scope checks
        ├── config.rs            # Settings, lock state, validation
        ├── privilege.rs         # OS detection + one-prompt elevation
        ├── hosts.rs             # Hosts-block rendering/staging (unit-tested)
        └── apps.rs              # Per-OS app blocking
```

## Setup

**Prerequisites:** [Rust](https://rustup.rs), Node 18+, and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev policykit-1`.

```bash
npm install
npx tauri icon path/to/square-1024.png   # once: generates src-tauri/icons (also used by the tray)
npm run dev                              # dev (autostart is NOT registered in dev)
npm run build                            # installers in src-tauri/target/release/bundle
cd src-tauri && cargo test               # unit tests
```

Install a **release build** for the auto-start behavior. On first launch the window opens for setup; after that it starts hidden at every login.

### Google sign-in

**Players** just click **Sign in with Google**; the app ships with the developer's own Google OAuth client built in.
**The developer** sets that up once, before building: see **[SETUP.md](SETUP.md)** (create the client in the Cloud Console,
copy `src-tauri/.cargo/config.toml.example` to `config.toml`, paste the two values, rebuild).

The app uses the installed-app flow with **PKCE and a loopback redirect**, with these scopes:

* `https://www.googleapis.com/auth/tasks` (read/write, needed to check quests off in Google Tasks)
* `https://www.googleapis.com/auth/calendar.readonly` (schedule times)
* `openid email` (only to show "Signed in as …")

Upgrading from a read-only build: tokens without the `tasks` scope are dropped and the app asks you to sign in once more.
Power users can override the built-in credentials under *System settings → Google account → Advanced*.

> While the consent screen is in *Testing*, Google expires refresh tokens after 7 days (the app notices and asks for a new
> sign-in). Publish it ("In production", unverified is fine for personal use) to avoid the weekly sign-in. Details in SETUP.md.

---

## Privilege model

The app never runs as root. It stages the hosts file with `std::fs` in a private (`0700`) folder and runs **one elevated script** through the native prompt: `pkexec` (Linux, with `sudo`-in-terminal fallback), `osascript … with administrator privileges` (macOS), or UAC via PowerShell (Windows). Expect **one prompt when the Gate seals and one when it opens**. A one-time backup lives at `hosts.system-gate.bak`.

## Limitations (please read)

* **An admin prompt at login.** Sealing needs elevation, so the first prompt appears shortly after you sign in. Cancelling it retries after 10 minutes. (A privileged helper service would avoid prompts; contributions welcome.)
* **Offline at login:** the Gate can't evaluate your quests without Google, so it stays open until the first successful check (it retries every 30 seconds).
* **First run / no Google connection:** nothing is sealed until Google is connected, otherwise there would be no way to ever unlock.
* **Quitting from the tray stops enforcement.** A sealed Gate stays sealed (hosts file remains), and it resumes at next login. Emergency release always works while the app runs.
* Hosts blocking is per-domain (no wildcards) and browsers using DNS-over-HTTPS can bypass it; sealing the browser app itself is the strongest option.
* Linux/macOS app blocking uses `chmod`, which can't stop Snap/Flatpak/read-only installs. Windows blocks the app's network access.
* Anyone with admin rights can undo this. It's a self-discipline tool, not a security boundary.
* **Manual recovery:** delete the `SYSTEM-GATE BEGIN/END` block from your hosts file (or restore `hosts.system-gate.bak`); `chmod +x` blocked binaries; on Windows `netsh advfirewall firewall delete rule name="SystemGate-<app>"`.
* Tokens are stored in a user-only file, not the OS keychain (swap in the `keyring` crate: good first PR).

## Contributing

Run `cargo fmt`, `cargo clippy` and `cargo test` before a PR. Ideas: keyring storage, privileged helper, secondary calendars, event-time matching beyond exact titles, Snap/Flatpak support.

## License

MIT; add a `LICENSE` file before publishing.
