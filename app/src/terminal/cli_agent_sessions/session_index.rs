//! Reads claude's on-disk session records to power the session navigator panel.
//!
//! claude is the source of truth for its own sessions:
//! - `~/.claude/sessions/*.json` — one file per *live* claude process
//!   (`{sessionId, cwd, status, startedAt, updatedAt, ...}`); `cwd` is the git
//!   worktree path when the session was started with `--worktree`.
//! - `~/.claude/history.jsonl` — append log of every prompt
//!   (`{display, project, sessionId, timestamp}`); used for a human-readable
//!   title, project/worktree grouping, and last-activity sort key.
//!
//! Parsing is split from IO so the merge logic is unit-testable with fixture
//! strings (no real `~/.claude` required). Whether a session is *open in Warp*
//! is NOT decided here — that's live state from `CLIAgentSessionsModel`,
//! computed at render time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde::Deserialize;

/// Max title length (in chars, not bytes — never split a UTF-8 boundary).
const MAX_TITLE_CHARS: usize = 80;

/// A claude session surfaced in the navigator.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSessionEntry {
    pub session_id: String,
    /// Human-readable label (the most recent prompt), or a fallback.
    pub title: String,
    /// The directory claude ran in (worktree path when applicable). May be empty.
    pub project_path: String,
    /// Latest activity in epoch ms (max of running `updatedAt` and history ts).
    pub last_active_ms: u64,
    /// A live claude process exists for this session (in Warp or an external
    /// terminal). "Open in Warp" is a separate, render-time check.
    pub is_running: bool,
}

#[derive(Deserialize)]
struct RunningSessionFile {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<u64>,
    #[serde(rename = "startedAt")]
    started_at: Option<u64>,
}

struct RunningAgg {
    cwd: Option<String>,
    last_active_ms: u64,
}

#[derive(Deserialize)]
struct HistoryLine {
    display: Option<String>,
    project: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    // Stored as a string of epoch ms, e.g. "1780988403875".
    timestamp: Option<String>,
}

struct HistoryAgg {
    title: String,
    project: Option<String>,
    last_active_ms: u64,
}

/// `CLAUDE_HOME` env var if set, else `~/.claude`. Mirrors
/// `plugin_manager::claude::claude_home_dir`.
pub fn claude_home() -> Option<PathBuf> {
    if let Ok(home) = env::var("CLAUDE_HOME") {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|home| home.join(".claude"))
}

/// Loads and merges claude sessions, newest first, capped to `cap`. Returns an
/// empty vec if `~/.claude` is missing or unreadable.
pub fn load_claude_sessions(claude_home: &Path, cap: usize) -> Vec<ClaudeSessionEntry> {
    let running_files = read_running_session_files(&claude_home.join("sessions"));
    let history_text = fs::read_to_string(claude_home.join("history.jsonl")).unwrap_or_default();
    merge_sessions(&running_files, &history_text, cap)
}

/// Reads every `*.json` under `sessions/` into a list of file contents. Missing
/// dir → empty. Unreadable individual files are skipped.
fn read_running_session_files(sessions_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("json"))
                .then(|| fs::read_to_string(&path).ok())
                .flatten()
        })
        .collect()
}

/// Pure merge: running session file contents + history.jsonl text → sorted,
/// de-duped, capped entries.
fn merge_sessions(running_files: &[String], history_text: &str, cap: usize) -> Vec<ClaudeSessionEntry> {
    let running = parse_running(running_files);
    let history = parse_history(history_text);

    let mut ids: Vec<String> = running.keys().chain(history.keys()).cloned().collect();
    ids.sort();
    ids.dedup();

    let mut entries: Vec<ClaudeSessionEntry> = ids
        .into_iter()
        .map(|session_id| {
            let run = running.get(&session_id);
            let hist = history.get(&session_id);

            let project_path = run
                .and_then(|r| r.cwd.clone())
                .or_else(|| hist.and_then(|h| h.project.clone()))
                .unwrap_or_default();

            let title = hist
                .map(|h| h.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| fallback_title(&project_path));

            let last_active_ms = run
                .map(|r| r.last_active_ms)
                .unwrap_or(0)
                .max(hist.map(|h| h.last_active_ms).unwrap_or(0));

            ClaudeSessionEntry {
                session_id,
                title,
                project_path,
                last_active_ms,
                is_running: run.is_some(),
            }
        })
        .collect();

    // Newest first; truncation is intentional and bounded.
    entries.sort_by(|a, b| b.last_active_ms.cmp(&a.last_active_ms));
    if entries.len() > cap {
        log::debug!(
            "claude session navigator: showing {cap} of {} sessions",
            entries.len()
        );
        entries.truncate(cap);
    }
    entries
}

fn parse_running(files: &[String]) -> HashMap<String, RunningAgg> {
    let mut out: HashMap<String, RunningAgg> = HashMap::new();
    for contents in files {
        let Ok(file) = serde_json::from_str::<RunningSessionFile>(contents) else {
            continue;
        };
        let Some(session_id) = file.session_id.filter(|s| !s.is_empty()) else {
            continue;
        };
        let last_active_ms = file.updated_at.or(file.started_at).unwrap_or(0);
        // A session id should appear once in sessions/, but if duplicated keep
        // the most recent.
        let agg = out.entry(session_id).or_insert(RunningAgg {
            cwd: file.cwd.clone(),
            last_active_ms,
        });
        if last_active_ms >= agg.last_active_ms {
            agg.last_active_ms = last_active_ms;
            if file.cwd.is_some() {
                agg.cwd = file.cwd;
            }
        }
    }
    out
}

fn parse_history(text: &str) -> HashMap<String, HistoryAgg> {
    let mut out: HashMap<String, HistoryAgg> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<HistoryLine>(line) else {
            continue;
        };
        let Some(session_id) = entry.session_id.filter(|s| !s.is_empty()) else {
            continue;
        };
        let ts = entry.timestamp.as_deref().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
        let title = entry.display.as_deref().map(clean_title).unwrap_or_default();

        match out.get_mut(&session_id) {
            // Keep the latest prompt as the title.
            Some(agg) if ts >= agg.last_active_ms => {
                agg.last_active_ms = ts;
                if !title.is_empty() {
                    agg.title = title;
                }
                if entry.project.is_some() {
                    agg.project = entry.project;
                }
            }
            Some(_) => {}
            None => {
                out.insert(
                    session_id,
                    HistoryAgg {
                        title,
                        project: entry.project,
                        last_active_ms: ts,
                    },
                );
            }
        }
    }
    out
}

/// Collapse a prompt into a one-line, length-bounded title.
fn clean_title(display: &str) -> String {
    let single_line: String = display.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() > MAX_TITLE_CHARS {
        let truncated: String = single_line.chars().take(MAX_TITLE_CHARS).collect();
        format!("{truncated}…")
    } else {
        single_line
    }
}

/// Title for a session with no usable prompt history: the last path component
/// of its project dir, or a generic label.
fn fallback_title(project_path: &str) -> String {
    project_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "(claude session)".to_string())
}

#[cfg(test)]
#[path = "session_index_tests.rs"]
mod tests;
