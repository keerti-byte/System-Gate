//! Google Tasks API client (read + complete).
//!
//! What counts as today's quest log:
//!   * pending tasks (`needsAction`) due today **or overdue**;
//!   * never completed tasks, never tasks due in the future, never old history.
//!
//! Completed-today tasks are fetched separately and only *counted* (progress
//! bar) and *awarded EXP*; they are never shown.

use crate::calendar;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Start/end (Unix seconds) of the calendar event a task is matched to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

/// What ticking a task will pay (preview for the UI; `rpg::RpgState::award` pays it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reward {
    pub exp: u64,
    /// Attribute labels fed ("MP", "HP", …). Empty = general EXP only.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// `"needsAction"` or `"completed"`.
    #[serde(default)]
    pub status: String,
    /// RFC 3339; Google only stores the *date*. The time part is always midnight UTC.
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub completed: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Filled in by us (never by Google): matching calendar event, if any.
    #[serde(default, skip_deserializing)]
    pub event: Option<Span>,
    /// Filled in by us: EXP and attributes this task will pay.
    #[serde(default, skip_deserializing)]
    pub reward: Option<Reward>,
}

impl Task {
    pub fn is_done(&self) -> bool {
        self.status == "completed"
    }
    /// `YYYY-MM-DD` part of the due date.
    pub fn due_date(&self) -> Option<&str> {
        self.due.as_deref().and_then(|d| d.get(..10))
    }
    /// Local calendar date on which the task was completed.
    pub fn completed_local_date(&self) -> Option<String> {
        let dt = DateTime::parse_from_rfc3339(self.completed.as_deref()?).ok()?;
        Some(dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
    }
}

/// Pending, titled tasks due on or before `today` (`YYYY-MM-DD`). Tasks with no
/// due date are ignored, as are blank-titled ones Google creates by accident.
pub fn pending_due_by(all: Vec<Task>, today: &str) -> Vec<Task> {
    all.into_iter()
        .filter(|t| !t.is_done() && !t.title.trim().is_empty() && t.due_date().is_some_and(|d| d <= today))
        .collect()
}

/// Tasks completed on the local date `today`.
pub fn completed_on(all: Vec<Task>, today: &str) -> Vec<Task> {
    all.into_iter()
        .filter(|t| t.is_done() && !t.title.trim().is_empty() && t.completed_local_date().as_deref() == Some(today))
        .collect()
}

pub struct Fetched {
    pub pending: Vec<Task>,
    pub done_today: Vec<Task>,
}

pub async fn fetch(token: &str, list_id: &str) -> Result<Fetched> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let url = format!("https://tasks.googleapis.com/tasks/v1/lists/{list_id}/tasks");

    let today_date = Local::now().date_naive();
    let today = today_date.format("%Y-%m-%d").to_string();
    let tomorrow = today_date.succ_opt().context("date overflow")?.format("%Y-%m-%d").to_string();
    let (midnight, _) = calendar::day_bounds()?;

    // 1) Pending only; `dueMax` drops future tasks server-side.
    let pending = list(
        &client,
        &url,
        token,
        vec![
            ("showCompleted", "false".into()),
            ("maxResults", "100".into()),
            ("dueMax", format!("{tomorrow}T00:00:00.000Z")),
        ],
    )
    .await?;
    // 2) Completed since local midnight: counted + awarded, never displayed.
    let completed = list(
        &client,
        &url,
        token,
        vec![
            ("showCompleted", "true".into()),
            ("showHidden", "true".into()),
            ("completedMin", midnight),
            ("maxResults", "100".into()),
        ],
    )
    .await?;

    Ok(Fetched { pending: pending_due_by(pending, &today), done_today: completed_on(completed, &today) })
}

async fn list(client: &reqwest::Client, url: &str, token: &str, params: Vec<(&str, String)>) -> Result<Vec<Task>> {
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..20 {
        // hard cap on pages so a huge history can never stall the poller
        let mut req = client.get(url).bearer_auth(token).query(&params);
        if let Some(p) = &page {
            req = req.query(&[("pageToken", p.as_str())]);
        }
        let resp = req.send().await.context("Could not reach the Google Tasks API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Google Tasks API returned {status}: {body}");
        }
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            items: Vec<Task>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
        }
        let body: Page = resp.json().await?;
        out.extend(body.items);
        match body.next_page_token {
            Some(t) => page = Some(t),
            None => break,
        }
    }
    Ok(out)
}

/// Task ids are URL-safe tokens; anything else never reaches a request path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Marks a task completed in Google Tasks (`PATCH …/tasks/{id}` with
/// `{"status":"completed"}`) and returns it. Needs the read/write `tasks` scope.
pub async fn complete(token: &str, list_id: &str, task_id: &str) -> Result<Task> {
    complete_at("https://tasks.googleapis.com", token, list_id, task_id).await
}

/// `complete` against any base URL (the tests point it at a local mock server).
async fn complete_at(base: &str, token: &str, list_id: &str, task_id: &str) -> Result<Task> {
    if !valid_id(task_id) {
        bail!("Invalid task id.");
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let url = format!("{base}/tasks/v1/lists/{list_id}/tasks/{task_id}");
    let resp = client
        .patch(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "status": "completed" }))
        .send()
        .await
        .context("Could not reach the Google Tasks API. The task was NOT checked off.")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            404 => bail!("That task no longer exists in Google Tasks. Check quests again."),
            403 if body.contains("insufficient") || body.contains("PERMISSION_DENIED") => {
                bail!("Google refused: this sign-in cannot edit tasks. Disconnect and sign in with Google again.")
            }
            _ => bail!("Google refused to complete the task ({status}): {body}"),
        }
    }
    let done: Task = resp.json().await?;
    if !done.is_done() {
        bail!("Google answered, but the task is not marked completed. Check quests again.");
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str, status: &str, due: Option<&str>) -> Task {
        Task {
            id: title.into(),
            title: title.into(),
            status: status.into(),
            due: due.map(String::from),
            completed: None,
            notes: None,
            event: None,
            reward: None,
        }
    }

    #[test]
    fn pending_means_due_today_or_overdue_and_not_completed() {
        let list = vec![
            task("today", "needsAction", Some("2026-10-16T00:00:00.000Z")),
            task("overdue", "needsAction", Some("2023-05-01T00:00:00.000Z")),
            task("future", "needsAction", Some("2026-11-03T00:00:00.000Z")),
            task("old done", "completed", Some("2023-05-01T00:00:00.000Z")),
            task("no due", "needsAction", None),
            task("  ", "needsAction", Some("2026-10-16T00:00:00.000Z")),
        ];
        let kept: Vec<_> = pending_due_by(list, "2026-10-16").into_iter().map(|t| t.title).collect();
        assert_eq!(kept, vec!["today", "overdue"]);
    }

    /// Proves the tick really reaches Google: a one-shot local server plays the
    /// Tasks API and the test inspects the exact request the app sent.
    #[tokio::test]
    async fn ticking_a_quest_sends_patch_status_completed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_lowercase();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let body = r#"{"id":"abc_1-2","title":"Gym","status":"completed","completed":"2026-10-16T09:00:00.000Z"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        let done = complete_at(&base, "tok123", "@default", "abc_1-2").await.unwrap();
        assert!(done.is_done());

        let sent = server.await.unwrap();
        let lower = sent.to_lowercase();
        assert!(sent.starts_with("PATCH /tasks/v1/lists/@default/tasks/abc_1-2 "), "request line was: {sent}");
        assert!(lower.contains("authorization: bearer tok123"));
        assert!(sent.contains(r#""status":"completed""#), "body was: {sent}");
    }

    #[tokio::test]
    async fn bad_ids_never_reach_the_network() {
        let r = complete_at("http://127.0.0.1:9", "t", "@default", "../lists").await;
        assert!(r.unwrap_err().to_string().contains("Invalid task id"));
    }
}
