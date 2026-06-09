use super::*;

fn running_file(session_id: &str, cwd: &str, updated_at: u64) -> String {
    format!(
        r#"{{"pid":123,"sessionId":"{session_id}","cwd":"{cwd}","startedAt":1,"status":"busy","updatedAt":{updated_at}}}"#
    )
}

fn history_line(session_id: &str, project: &str, display: &str, ts: u64) -> String {
    format!(
        r#"{{"display":"{display}","pastedContents":"{{}}","project":"{project}","sessionId":"{session_id}","timestamp":"{ts}"}}"#
    )
}

#[test]
fn history_keeps_latest_prompt_as_title() {
    let text = [
        history_line("s1", "/repo", "first prompt", 100),
        history_line("s1", "/repo", "newest prompt", 300),
        history_line("s1", "/repo", "middle prompt", 200),
    ]
    .join("\n");

    let hist = parse_history(&text);
    let agg = hist.get("s1").expect("s1 present");
    assert_eq!(agg.title, "newest prompt");
    assert_eq!(agg.last_active_ms, 300);
    assert_eq!(agg.project.as_deref(), Some("/repo"));
}

#[test]
fn merge_marks_running_and_sorts_newest_first() {
    // s_run: running (worktree cwd) + history. s_hist: history only.
    let running = vec![running_file(
        "s_run",
        "/Users/me/repo/.claude/worktrees/feat",
        500,
    )];
    let history = [
        history_line("s_run", "/Users/me/repo/.claude/worktrees/feat", "worktree work", 450),
        history_line("s_hist", "/Users/me/other", "old session", 200),
    ]
    .join("\n");

    let entries = merge_sessions(&running, &history, 10);

    assert_eq!(entries.len(), 2);
    // Newest first.
    assert_eq!(entries[0].session_id, "s_run");
    assert!(entries[0].is_running);
    assert_eq!(entries[0].title, "worktree work");
    assert_eq!(entries[0].project_path, "/Users/me/repo/.claude/worktrees/feat");
    assert_eq!(entries[0].last_active_ms, 500); // max(running 500, history 450)

    assert_eq!(entries[1].session_id, "s_hist");
    assert!(!entries[1].is_running);
}

#[test]
fn merge_caps_and_dedups() {
    let history = (0..5)
        .map(|i| history_line(&format!("s{i}"), "/r", "p", (i as u64 + 1) * 10))
        .collect::<Vec<_>>()
        .join("\n");
    // Same id in running and history must collapse to one entry.
    let running = vec![running_file("s4", "/r", 999)];

    let entries = merge_sessions(&running, &history, 3);
    assert_eq!(entries.len(), 3, "capped to 3");
    assert_eq!(entries[0].session_id, "s4", "highest last_active first");
    assert!(entries[0].is_running);
    // No duplicate s4.
    assert_eq!(entries.iter().filter(|e| e.session_id == "s4").count(), 1);
}

#[test]
fn malformed_lines_and_files_are_skipped() {
    let history = [
        "not json at all",
        "",
        &history_line("ok", "/r", "good", 100),
        r#"{"display":"no session id","timestamp":"50"}"#,
    ]
    .join("\n");
    let running = vec!["{ broken".to_string(), running_file("run", "/r", 100)];

    let entries = merge_sessions(&running, &history, 10);
    let ids: Vec<&str> = entries.iter().map(|e| e.session_id.as_str()).collect();
    assert!(ids.contains(&"ok"));
    assert!(ids.contains(&"run"));
    assert_eq!(entries.len(), 2);
}

#[test]
fn fallback_title_uses_worktree_dir_name() {
    let running = vec![running_file("s", "/Users/me/repo/.claude/worktrees/my-feat", 100)];
    let entries = merge_sessions(&running, "", 10);
    assert_eq!(entries[0].title, "my-feat");
}

#[test]
fn clean_title_collapses_whitespace_and_truncates() {
    assert_eq!(clean_title("  multi   line\n prompt "), "multi line prompt");
    let long = "x".repeat(200);
    let cleaned = clean_title(&long);
    assert_eq!(cleaned.chars().count(), MAX_TITLE_CHARS + 1); // + the ellipsis
    assert!(cleaned.ends_with('…'));
}
