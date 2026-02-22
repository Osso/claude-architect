use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum Request {
    Validate {
        project: String,
        goal: String,
        tasks: Vec<String>,
    },
    Ping,
}

#[derive(Serialize, Deserialize)]
pub enum Response {
    Verdict(String),
    Pong,
    Error(String),
}

pub fn socket_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .expect("XDG_RUNTIME_DIR must be set");
    format!("{dir}/claude-architect.sock")
}

/// Subagent types that are read-only — skip architect validation.
pub const EXPLORATION_AGENTS: &[&str] = &[
    "Explore",
    "Plan",
    "feature-dev:code-explorer",
    "feature-dev:code-reviewer",
    "feature-dev:code-architect",
    "architect",
    "claude-code-guide",
    "statusline-setup",
];
