# Claude session navigator (left-panel tool)

**Date:** 2026-06-09
**Status:** Approved design, ready for implementation
**Branch:** `feat/claude-session-navigator`

## Problem

You can have several `claude` CLI sessions running across Warp panes, tabs, windows,
and git worktrees. There's no single place to see them or jump between them. claude's
own `claude --resume` / `/resume` only covers the current directory and lives in the
terminal, not as an always-visible overview.

## Goal

A persistent left-panel tool that **lists claude sessions**, **marks the ones open in
a Warp pane**, and on click **jumps to that pane's window/tab**. Clicking a session
that isn't open in Warp does nothing (by design — this is a navigator, not a resumer).

Local-only, no backend, no plugin.

## Why this is Warp's job, not a plugin's

Two of the three core behaviors are impossible for a Claude Code plugin:
- **Marking "open in Warp"** needs the pane↔session map, which only Warp has
  (`CLIAgentSessionsModel`).
- **Jumping to a pane/window** is a Warp-internal action no external process can do.

A plugin only earns its place when it supplies data Warp can't otherwise get (the
existing `claude-code-warp` plugin does this for live status via OSC). The navigator's
data is all locally readable, so Warp reads it directly.

## Data sources

| Source | Content | Use |
|--------|---------|-----|
| `~/.claude/sessions/*.json` | Live claude processes: `{sessionId, cwd, status, startedAt, updatedAt, kind}` | The "running" set; `cwd` is the worktree path when applicable |
| `~/.claude/history.jsonl` | Append log of every prompt: `{display, project, sessionId, timestamp}` | Recent history: human-readable title (last `display`), project/worktree grouping, sort key |
| `CLIAgentSessionsModel` (in-memory) | `terminal_view_id → CLIAgentSession{ session_context.session_id }` | Which sessions are open in a Warp pane + the view to jump to |

Scope: **running + recent history** (merged & de-duped by `sessionId`). History is
capped to a recent window (e.g. last 50 sessions by latest timestamp) — `log` what's
truncated rather than silently cutting.

## Architecture

### Data layer (claude-agnostic file reads, in the CLI agent subsystem)
A new module under `cli_agent_sessions` (e.g. `session_index.rs`) that:
- Reads `~/.claude/sessions/*.json` → running sessions.
- Reads (tails) `~/.claude/history.jsonl` → groups prompts by `sessionId`, keeps the
  latest `display` as title, `project` as path, max `timestamp` as last-activity.
- Merges into a `Vec<ClaudeSessionEntry { session_id, title, project_path, last_active,
  is_running }>`, sorted by `last_active` desc, capped.
- Pure parsing functions are unit-testable with fixture strings (no real `~/.claude`).

`CLAUDE_HOME` env override honored (matches `plugin_manager/claude.rs` which already
resolves `~/.claude` via `CLAUDE_HOME`).

### "Open in Warp" + jump (Warp-internal, in the subsystem)
- `CLIAgentSessionsModel::find_view_for_session_id(&str) -> Option<EntityId>` — iterate
  `sessions` and match `session_context.session_id`. Keeps the lookup in the subsystem
  (same closed-loop placement as `resume_descriptor`).
- Click handler: `find_view_for_session_id` → `focus_terminal_view_locally`, falling
  back to `focus_terminal_view_in_other_window` (`workspace/view.rs:5634/5660`, both
  already exist).

### UI (new left-panel tool)
- New `ToolPanelView::ClaudeSessions` variant (left_panel.rs + `compute_left_panel_views`
  in workspace/view.rs), gated by `cfg!(feature = "local_fs")` (needs disk reads),
  alongside Project Explorer / Global Search. No login/AI gate (unlike the existing
  cloud Conversation List).
- A `ClaudeSessionsView` listing entries: title, project/worktree path, relative time,
  and an "open in Warp" marker (icon/highlight) for running-in-Warp ones. Optional
  grouping by project/worktree.
- Row click: if open in Warp → jump; else inert.
- Refresh: re-read `~/.claude` on panel open + a light periodic/′on-focus refresh;
  update markers when `CLIAgentSessionsModel` changes.
- Follow `warp-ui-guidelines` for the view (consult before writing UI code).

## Edge cases

- `~/.claude` missing / unreadable / malformed lines → skip that source, empty list, no
  crash (parse defensively, per-line).
- Session in `history.jsonl` but its process gone → shown, `is_running = false`, not
  marked, inert on click.
- Running session not hosted by Warp (external terminal/tmux) → shown as running but no
  Warp marker; click inert (we can't focus a non-Warp terminal).
- Same `sessionId` in multiple sources → merged once; running status from
  `sessions/*.json`, title/time enriched from `history.jsonl`.
- Stale view: a session's pane closed since last read → marker recomputed from live
  `CLIAgentSessionsModel`, not cached.

## Testing

- Unit: `history.jsonl` parsing/grouping (title = latest display, path, sort), merge &
  de-dup with `sessions/*.json`, truncation cap, malformed-line resilience — all with
  fixture strings.
- Unit: `find_view_for_session_id` matches the right view; `None` when absent.
- Manual: run claude in two worktrees + one plain pane (+ one in an external terminal);
  open the panel; confirm list, markers, and that clicking a Warp-hosted one jumps to
  the right window/tab while an external/historical one is inert.

## Out of scope (YAGNI)

- Resuming/opening a non-open session (new pane / `claude --resume`) — explicitly inert.
- Deleting/renaming sessions, search/filter, cross-machine, transcript preview.
- Reusing the restart-resume `AgentResume` data — that's per-pane persistence for a
  different feature; the navigator reads `~/.claude` directly.

## Key risk (resolved)

Cross-window/tab pane focus — confirmed available: `focus_terminal_view_locally` /
`focus_terminal_view_in_other_window` already implement exactly this.
