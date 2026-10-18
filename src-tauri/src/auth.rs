//! Google OAuth2 for installed apps: Authorization Code + PKCE + loopback redirect.
//!
//! Whose Google credentials? The DEVELOPER's. They are compiled into the app at
//! build time (`SYSTEM_GATE_GOOGLE_CLIENT_ID` / `_SECRET`, see SETUP.md), so an end
//! user only clicks "Sign in with Google" and never sees the Cloud Console. For a
//! desktop app the client secret is not confidential (Google documents this); the
//! protection is PKCE plus the loopback redirect. Advanced users may override the
//! credentials in System settings.
//!
//! Flow:
//!  1. Bind a one-shot HTTP listener on 127.0.0.1:<random port>.
//!  2. Open Google's consent page in the default browser (scopes: tasks (read/write, to check off quests) + calendar.readonly).
//!  3. Google redirects to the loopback address with `?code=…&state=…`.
//!  4. Exchange the code (+ PKCE verifier) for access and refresh tokens.
//!  5. Persist tokens (0600 on Unix); refresh silently when they expire.

use crate::config::{restrict, Config, Dirs};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// `openid email` only adds the account's address (shown as "Signed in as …").
const SCOPE: &str =
    "openid email https://www.googleapis.com/auth/tasks https://www.googleapis.com/auth/calendar.readonly";

/// Compiled in by `cargo` from the environment / `.cargo/config.toml` (see SETUP.md).
const EMBEDDED_CLIENT_ID: Option<&str> = option_env!("SYSTEM_GATE_GOOGLE_CLIENT_ID");
const EMBEDDED_CLIENT_SECRET: Option<&str> = option_env!("SYSTEM_GATE_GOOGLE_CLIENT_SECRET");

/// (client id, client secret) to use. A client id set in Advanced settings wins
/// (together with its own secret); otherwise the credentials built into this app.
fn credentials(cfg: &Config) -> Result<(String, String)> {
    if !cfg.google_client_id.is_empty() {
        return Ok((cfg.google_client_id.clone(), cfg.google_client_secret.clone()));
    }
    let id = EMBEDDED_CLIENT_ID.unwrap_or("").trim().to_string();
    if id.is_empty() || id.contains("PASTE") {
        bail!(
            "This build of System Gate has no Google credentials. The developer must set them once \
             before building (see SETUP.md), or you can enter your own under System settings → Advanced."
        );
    }
    Ok((id, EMBEDDED_CLIENT_SECRET.unwrap_or("").trim().to_string()))
}

/// Google lets users untick individual permissions on the consent screen,
/// so the granted scopes must be checked, not assumed.
fn has_all_scopes(granted: &str) -> bool {
    // Exact match: "…/auth/tasks" is a prefix of "…/auth/tasks.readonly", so `contains` would be wrong.
    let granted: Vec<&str> = granted.split_whitespace().collect();
    granted.contains(&"https://www.googleapis.com/auth/tasks")
        && granted.contains(&"https://www.googleapis.com/auth/calendar.readonly")
}

#[derive(Serialize, Deserialize, Clone)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    /// Space-separated scopes Google granted. Missing in tokens saved by older versions.
    #[serde(default)]
    scope: String,
    /// Account address, for display only.
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
    id_token: Option<String>,
}

/// Reads the `email` claim of an ID token. It came straight from Google over TLS
/// and is only displayed, so the signature is not checked.
fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("email")?.as_str().map(String::from)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_string(n: usize) -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(n).map(char::from).collect()
}

/// True only if tokens exist AND cover both Tasks and Calendar.
pub fn is_authorized(dirs: &Dirs) -> bool {
    load_tokens(dirs).map(|t| has_all_scopes(&t.scope)).unwrap_or(false)
}

/// The signed-in Google account, if known.
pub fn account(dirs: &Dirs) -> Option<String> {
    load_tokens(dirs).ok().and_then(|t| t.email)
}

pub fn logout(dirs: &Dirs) {
    let _ = fs::remove_file(dirs.tokens());
}

/// Runs the interactive sign-in. Returns once tokens are stored.
pub async fn login(app: &AppHandle, dirs: &Dirs, cfg: &Config) -> Result<()> {
    let (client_id, client_secret) = credentials(cfg)?;
    let verifier = random_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_string(24);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

    let url = url::Url::parse_with_params(
        AUTH_URL,
        &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("access_type", "offline"), // ask for a refresh token
            ("prompt", "consent"),
        ],
    )?;
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|e| anyhow!("Could not open the browser: {e}"))?;

    let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&listener, &state))
        .await
        .map_err(|_| anyhow!("Timed out waiting for Google sign-in (5 minutes)"))??;

    let mut form = vec![
        ("code", code.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("grant_type", "authorization_code"),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret.as_str()));
    }
    let resp = request_token(&form).await?;
    let refresh_token = resp.refresh_token.ok_or_else(|| {
        anyhow!("Google returned no refresh token. Remove this app at myaccount.google.com/permissions and sign in again.")
    })?;
    let scope = resp.scope.unwrap_or_default();
    if !has_all_scopes(&scope) {
        bail!("Both permissions are required (manage Google Tasks, and view Google Calendar). Authorize again and leave both boxes ticked.");
    }
    let email = resp.id_token.as_deref().and_then(email_from_id_token);
    save_tokens(
        dirs,
        &Tokens { access_token: resp.access_token, refresh_token, expires_at: now() + resp.expires_in, scope, email },
    )
}

/// Returns a valid access token, refreshing it if it is about to expire.
pub async fn access_token(dirs: &Dirs, cfg: &Config) -> Result<String> {
    let mut t = load_tokens(dirs)?;
    if !has_all_scopes(&t.scope) {
        logout(dirs);
        bail!("System Gate needs new permissions (checking off tasks, reading Calendar). Click “Sign in with Google” again.");
    }
    if t.expires_at > now() + 60 {
        return Ok(t.access_token);
    }
    let (client_id, client_secret) = credentials(cfg)?;
    let refresh = t.refresh_token.clone();
    let mut form = vec![
        ("refresh_token", refresh.as_str()),
        ("client_id", client_id.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret.as_str()));
    }
    let r = match request_token(&form).await {
        Ok(r) => r,
        Err(e) if format!("{e:#}").contains("invalid_grant") => {
            // Revoked, expired (Google "Testing" apps expire after 7 days) or password changed.
            logout(dirs);
            bail!("Your Google sign-in expired or was revoked. Click “Sign in with Google” again.");
        }
        Err(e) => return Err(e),
    };
    t.access_token = r.access_token;
    t.expires_at = now() + r.expires_in;
    if let Some(new_refresh) = r.refresh_token {
        t.refresh_token = new_refresh;
    }
    save_tokens(dirs, &t)?;
    Ok(t.access_token)
}

async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");

        let (mut code, mut state, mut error) = (None, None, None);
        if let Ok(u) = url::Url::parse(&format!("http://127.0.0.1{path}")) {
            for (k, v) in u.query_pairs() {
                match k.as_ref() {
                    "code" => code = Some(v.into_owned()),
                    "state" => state = Some(v.into_owned()),
                    "error" => error = Some(v.into_owned()),
                    _ => {}
                }
            }
        }
        // Browsers sometimes probe /favicon.ico first; ignore anything without a result.
        if code.is_none() && error.is_none() {
            respond(&mut stream, "404 Not Found", "Not found").await;
            continue;
        }
        if state.as_deref() != Some(expected_state) {
            respond(&mut stream, "400 Bad Request", "State mismatch. Close this tab and retry.").await;
            bail!("OAuth state mismatch (possible forged redirect)");
        }
        if let Some(e) = error {
            respond(&mut stream, "200 OK", "Authorization denied. You can close this tab.").await;
            bail!("Google sign-in was denied: {e}");
        }
        respond(&mut stream, "200 OK", "Authorization complete. You can close this tab.").await;
        return code.ok_or_else(|| anyhow!("missing authorization code"));
    }
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>System</title>\
         <body style=\"background:#060a14;color:#9fd2ff;font-family:sans-serif;padding:3rem\">\
         <h2>[ SYSTEM ] {message}</h2>"
    );
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn request_token(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = reqwest::Client::new().post(TOKEN_URL).form(form).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Google token endpoint returned {status}: {body}");
    }
    Ok(resp.json().await?)
}

fn load_tokens(dirs: &Dirs) -> Result<Tokens> {
    let text = fs::read_to_string(dirs.tokens())
        .map_err(|_| anyhow!("Not signed in yet. Click “Sign in with Google”."))?;
    Ok(serde_json::from_str(&text)?)
}

fn save_tokens(dirs: &Dirs, t: &Tokens) -> Result<()> {
    let path = dirs.tokens();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(t)?)?;
    restrict(&path, 0o600)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_tasks_scope_is_not_enough() {
        let ro = "openid email https://www.googleapis.com/auth/tasks.readonly https://www.googleapis.com/auth/calendar.readonly";
        assert!(!has_all_scopes(ro));
        let rw = "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/tasks https://www.googleapis.com/auth/calendar.readonly";
        assert!(has_all_scopes(rw));
        assert!(!has_all_scopes("https://www.googleapis.com/auth/tasks"));
    }

    #[test]
    fn email_is_read_from_the_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"me@example.com","sub":"1"}"#);
        assert_eq!(email_from_id_token(&format!("x.{payload}.y")).as_deref(), Some("me@example.com"));
        assert_eq!(email_from_id_token("garbage"), None);
    }

    #[test]
    fn user_override_wins_over_embedded_credentials() {
        let mut cfg = Config::default();
        cfg.google_client_id = "mine.apps.googleusercontent.com".into();
        cfg.google_client_secret = "s3cret".into();
        assert_eq!(credentials(&cfg).unwrap(), ("mine.apps.googleusercontent.com".into(), "s3cret".into()));
    }

    /// With credentials compiled in, a plain "Sign in" needs nothing from the user.
    /// Without them (a dev build), the error says what the developer must do.
    #[test]
    fn built_in_credentials_work_without_any_user_setup() {
        let r = credentials(&Config::default());
        match EMBEDDED_CLIENT_ID.map(str::trim) {
            Some(id) if !id.is_empty() && !id.contains("PASTE") => {
                let (got, _) = r.expect("embedded credentials should be used");
                assert_eq!(got, id);
            }
            _ => assert!(r.unwrap_err().to_string().contains("SETUP.md")),
        }
    }
}
