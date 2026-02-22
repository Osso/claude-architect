# claude-architect

A task decomposition validator for Claude Code. Runs as a daemon and intercepts `Task()` agent spawns to validate scope, conflicts, and ordering before execution.

## How It Works

```
Task() call → PreToolUse hook → socket daemon → claude CLI → verdict
                                                              ├─ ok → allow
                                                              └─ needs-changes → deny with feedback
```

1. Claude Code's PreToolUse hook (`claude-architect-hook`) intercepts `Task()` calls
2. Read-only agent types (Explore, Plan, etc.) are allowed through without validation
3. Implementation agents are forwarded to the daemon via Unix socket
4. The daemon calls `claude` CLI with a persistent session per project
5. On `VERDICT: needs-changes`, the hook blocks the Task() call with feedback
6. On `VERDICT: ok` or any error, the call proceeds

## Components

| Binary | Role |
|--------|------|
| `claude-architect` | Socket daemon — validates tasks via `claude` CLI |
| `claude-architect-mcp` | MCP server — exposes `architect_validate` tool |
| `claude-architect-hook` | PreToolUse hook — intercepts `Task()` calls |

## Design Documents

The daemon maintains per-project design documents that capture architectural understanding:

- Loaded from `~/.local/share/claude-architect/designs/{project}.md`
- Injected into the system prompt on the first validation of each session
- Regenerated every 20 validations by asking Claude to summarize its accumulated context

This provides architectural awareness without re-reading the codebase on every validation.

## Setup

### Build

```bash
cargo build --release
```

### Install the daemon

Create a systemd user service:

```ini
# ~/.config/systemd/user/claude-architect.service
[Unit]
Description=Claude Architect validation daemon

[Service]
ExecStart=%h/.cargo/bin/claude-architect
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
cargo install --path .
systemctl --user enable --now claude-architect
```

### Configure the MCP server

Add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "claude-architect": {
      "command": "claude-architect-mcp"
    }
  }
}
```

### Configure the hook

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Task",
        "command": "claude-architect-hook"
      }
    ]
  }
}
```

### System prompt

Place the architect instructions at `~/.claude/agents/architect.md` with YAML frontmatter. The daemon strips the frontmatter and uses the body as the system prompt for validation calls.

## Data Storage

```
~/.local/share/claude-architect/
├── sessions.json              # Session IDs + validation counters per project
└── designs/
    ├── globalcomix.md          # Auto-generated design docs
    ├── sakuin.md
    └── ...
```

## Dependencies

- [peercred-ipc](../../lib/peercred-ipc) — Unix socket IPC with SO_PEERCRED
- [rmcp](https://crates.io/crates/rmcp) — MCP server framework
- `claude` CLI — called as subprocess for validation
