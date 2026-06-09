# Resume Claude sessions on Warp restart

**Date:** 2026-06-09
**Status:** Approved design, ready for implementation plan

## Problem

When Warp restarts with session restoration on (`general.restore_session`, default
`true`), it restores windows, tabs, the split layout, each terminal pane's `cwd`,
command history, and shell type. But it spawns a **fresh shell** at the saved cwd —
it does not re-launch whatever foreground program was running.

So a pane that was running an interactive `claude` conversation comes back as a bare
shell in the right directory. The conversation is gone; you land in the cwd, not the
session. We want the pane to come back **in the Claude session**, the way
`~/src/code-cave` re-launches `claude --resume <id>` on restart.

## Key insight: Warp already captures the session id

We do **not** need code-cave's output sniffer or filesystem poller. Warp already
tracks, per terminal view, a live `CLIAgentSessionContext { cwd, session_id, ... }`:

- The first-party Claude plugin emits OSC 777 `SessionStart` events carrying
  `session_id` and `cwd`.
- `CLIAgentSessionListener` (`app/src/terminal/cli_agent_sessions/listener/mod.rs`)
  forwards those to `CLIAgentSessionsModel`.
- `CLIAgentSessionsModel::session(terminal_view_id)` returns
  `CLIAgentSession { agent: CLIAgent, session_context: CLIAgentSessionContext, .. }`
  (`app/src/terminal/cli_agent_sessions/mod.rs:38-41, 117-148`).

So the feature collapses to: **persist what Warp already knows on shutdown, and
replay it on restore.**

## Decisions (settled with the user)

- **Resume strategy:** mirror code-cave — prefer `claude --resume <id>` using the
  captured `session_id`; fall back to `claude --continue` only when no id was
  captured. `--resume <id>` is deterministic (targets the exact conversation);
  `--continue` reopens the most-recent conversation in the cwd, which can be the
  wrong one if multiple conversations exist for that directory.
- **Always add `--dangerously-skip-permissions`** to the relaunch command, matching
  code-cave's `build_claude` (`~/src/code-cave/src-tauri/src/agents.rs:15-17`).
- **Auto-execute** the command in the restored pane (not pre-fill).
- **Claude only**, **always-on**, **no feature flag** — keep the fork minimal.

## Architecture

Data flow (only the last three steps are new):

```
Claude plugin → OSC 777 SessionStart → CLIAgentSessionListener
  → CLIAgentSessionsModel.session_context.{session_id, cwd}      [exists today]
  → TerminalPane::snapshot() reads it on quit                    [NEW]
  → terminal_panes.agent_resume_json (nullable TEXT column)      [NEW]
  → restore path reads it on launch → auto-runs claude --resume  [NEW]
```

### 1. New persisted field

- `TerminalPaneSnapshot` (`app/src/app_state.rs:204-219`) gains
  `agent_resume: Option<AgentResume>`.
- `AgentResume { session_id: Option<String> }`. Claude-only, so no `agent` enum is
  stored; the relaunch cwd is the pane's own restored cwd, so cwd is not duplicated
  here.
- Persisted as a nullable `agent_resume_json TEXT` column on the `terminal_panes`
  table: schema (`crates/persistence/src/schema.rs`), Diesel model
  (`crates/persistence/src/model.rs`), a migration, and the read/write paths in
  `app/src/persistence/sqlite.rs`. JSON keeps it forward-compatible (e.g. adding
  other agents later) without more columns.

### 2. Capture on save

In `TerminalPane::snapshot` (`app/src/pane_group/pane/terminal_pane.rs:488`, the main
non-viewer `else` branch that builds the populated `TerminalPaneSnapshot`):

```rust
let agent_resume = CLIAgentSessionsModel::as_ref(app)
    .session(self.terminal_view(app).id())
    .filter(|session| session.agent == CLIAgent::Claude)
    .map(|session| AgentResume {
        session_id: session.session_context.session_id.clone(),
    });
```

The other snapshot branches (shared-session viewer, ambient agent, transcript viewer)
set `agent_resume: None` — they already build minimal snapshots, so behavior there is
unchanged.

### 3. Replay on restore

In the restore path (`app/src/pane_group/mod.rs`, immediately after the
`create_session(...)` call around line 1639):

- Compute the relaunch command only if **both** are true:
  - `terminal_snapshot.agent_resume.is_some()`, and
  - `startup_directory.is_some()` (the saved cwd still exists — already filtered at
    `mod.rs:1594-1597`). If the dir is gone we skip the resume and leave a plain shell
    rather than resuming in the wrong directory.
- Auto-execute via the existing pending-command mechanism used for Oz startup
  commands (`set_pending_command_queue` at `mod.rs:1360`, or
  `execute_command_or_set_pending`). The command runs in the pane's shell at the
  restored cwd.

### 4. Command builder (pure, unit-testable)

```rust
/// Mirrors code-cave's build_claude. Always skips permission prompts.
fn build_claude_resume(session_id: Option<&str>) -> String {
    match session_id {
        Some(id) => format!("claude --resume {id} --dangerously-skip-permissions"),
        None      => "claude --continue --dangerously-skip-permissions".to_string(),
    }
}
```

(Final argument ordering/quoting to be confirmed against actual `claude` CLI during
implementation; `session_id` is a UUID from the plugin so no shell-escaping concern,
but the implementation should still treat it defensively.)

## Edge cases

| Case | Behavior |
|------|----------|
| No `session_id` captured (plugin absent / id not seen) | Fall back to `claude --continue …` |
| Saved cwd no longer exists | Skip resume; plain shell at home (today's fallback) |
| Pane was not running Claude / no agent | `agent_resume = None` → bare shell, unchanged |
| Shared-session viewer / ambient / transcript panes | `agent_resume = None` (those branches) |
| `general.restore_session` is off | Entire restore path skipped → no change |
| Non-Claude agent (codex, gemini, …) | Out of scope; `agent_resume = None` |

## Testing

- **Unit — capture:** `snapshot()` produces `Some(AgentResume { session_id })` when
  `CLIAgentSessionsModel` has a Claude session for the view; `None` for non-Claude /
  no session.
- **Unit — builder:** `build_claude_resume(Some("uuid"))` → `--resume uuid
  --dangerously-skip-permissions`; `build_claude_resume(None)` → `--continue
  --dangerously-skip-permissions`.
- **Persistence round-trip:** write a pane with `agent_resume_json` set, read it back,
  assert equality; and a NULL/legacy row reads back as `None`.
- **Integration:** model on `crates/integration/src/test/pane_restoration.rs` — set up
  a pane with a Claude session, snapshot + restore, assert the restored pane
  auto-executes the expected command. Wire into the manual runner / nextest suite per
  the `warp-integration-test` skill.

## Architecture revision (post-review)

After first implementation, two changes were made:

1. **Centralized in the CLI agent subsystem.** Claude-specific knowledge no longer
   lives in `pane_group`/`terminal_pane`. `AgentResume`, its `resume_command()`
   builder, and `CLIAgentSessionsModel::resume_descriptor(view_id)` (which decides
   what's resumable and captures it) all live in
   `app/src/terminal/cli_agent_sessions/mod.rs`. The snapshot path calls
   `resume_descriptor(...)`; the restore path calls `resume_command()`. Neither
   `pane_group` nor `terminal_pane` contains any Claude command strings — the seam
   is an opaque descriptor + a command string, ready for other agents to join via
   `resume_command`.

2. **Worktree handling via the agent's own cwd.** The original assumption — "the
   pane's restored cwd covers worktrees" — was wrong: a pane's shell cwd and the
   directory Claude actually ran in can diverge (git worktrees). `AgentResume` now
   carries `cwd` (from `session_context.cwd`, the plugin-reported working dir). On
   restore, the pane is launched in the agent's cwd when present (falling back to
   the pane's saved cwd), so `--resume`/`--continue` resolves the right
   conversation in the worktree.

## Out of scope (YAGNI)

- Other CLI agents (codex, gemini, …) — `resume_descriptor`/`resume_command` are
  the extension seam, but we ship Claude only first.
- Feature flag / setting toggle — always-on.
- Pre-fill (type-but-don't-run) mode.

## Reference

- code-cave: `~/src/code-cave/src-tauri/src/agents.rs` (`build_claude`),
  `~/src/code-cave/src-tauri/src/commands/agents.rs` (capture + replay).
