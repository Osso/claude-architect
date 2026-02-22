# claude-architect

Task decomposition validator for Claude Code. Intercepts `Task()` calls and validates them before execution.

## Architecture

Three binaries sharing `src/lib.rs` (IPC types + constants):

- **`claude-architect`** (`src/bin/server.rs`) — Socket daemon. Receives validation requests via `peercred-ipc`, calls `claude` CLI with session continuity per project, returns verdicts.
- **`claude-architect-mcp`** (`src/bin/mcp.rs`) — MCP server (stdio transport) exposing `architect_validate` tool. Forwards to socket daemon.
- **`claude-architect-hook`** (`src/bin/hook.rs`) — PreToolUse hook. Intercepts `Task()` calls, skips read-only agent types (`EXPLORATION_AGENTS`), sends the rest to the daemon for validation.

## Key Paths

- Socket: `$XDG_RUNTIME_DIR/claude-architect.sock`
- Data: `~/.local/share/claude-architect/`
  - `sessions.json` — persisted session IDs + validation counters
  - `designs/{project}.md` — auto-generated design documents
- System prompt: `~/.claude/agents/architect.md`

## Sandboxing

The `claude` CLI subprocess runs with `--permission-mode dontAsk` and an explicit `--allowedTools` whitelist: Read, Glob, Grep, Bash, and the `claude-memory` MCP tools. Bash access is guarded by the `claude-bash-hook` (inherited from `~/.claude/settings.json`). The architect cannot write files or use tools outside the whitelist.

## Project Access

The `Request::Validate` message includes a `cwd` field (populated by the hook from `$PWD`, or by the MCP tool from the `cwd` param). The daemon sets `current_dir(cwd)` on the `claude` subprocess so the architect can read project files.

## Design Document Lifecycle

1. **Referenced on session creation** — the system prompt tells the architect to read `designs/{project}.md` via the Read tool. If the file doesn't exist, the architect proceeds without it.
2. **Auto-generated if missing** — after the first validation, if no design doc exists, the daemon requests one from Claude and writes it to disk.
3. **Not re-injected on resume** — subsequent validations use `--resume` (Claude has session context).
4. **Session reset every 20 validations** — daemon asks Claude to summarize its architecture understanding, saves to disk, then resets the session (new UUID, `created = false`, counter to 0). The next validation starts fresh with the new design doc, preventing auto-compaction from degrading context.

## Concurrency

Per-project `Mutex<SessionInfo>` serializes validations for the same project. Different projects validate concurrently.

## Dependencies

- `peercred-ipc` — local library at `../../lib/peercred-ipc`
- `rmcp` — MCP server framework
- `claude` CLI — called as subprocess for validation
