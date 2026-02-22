use claude_architect::{EXPLORATION_AGENTS, Request, Response, socket_path};
use peercred_ipc::Client;
use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();

    let json: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return, // Not JSON, allow
    };

    let tool_name = json
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tool_name != "Task" {
        return; // Only intercept Task() calls
    }

    let tool_input = match json.get("tool_input") {
        Some(v) => v,
        None => return,
    };

    let subagent_type = tool_input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if should_skip(subagent_type) {
        return;
    }

    let description = tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = tool_input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let project = derive_project();
    let prompt_summary = truncate(prompt, 2000);

    let request = Request::Validate {
        project,
        goal: description.to_string(),
        tasks: vec![prompt_summary],
    };

    let path = socket_path();
    let response = match Client::call::<_, Request, Response>(&path, &request) {
        Ok(r) => r,
        Err(e) => {
            // Service unavailable — allow the task through
            eprintln!("claude-architect-hook: service error: {e}");
            return;
        }
    };

    match response {
        Response::Verdict(verdict) => {
            if contains_needs_changes(&verdict) {
                deny(&verdict);
            }
            // VERDICT: ok — allow through (exit 0)
        }
        Response::Error(e) => {
            eprintln!("claude-architect-hook: architect error: {e}");
            // Allow through on error
        }
        Response::Pong => {}
    }
}

fn should_skip(subagent_type: &str) -> bool {
    if subagent_type.is_empty() {
        return false; // Unknown type, validate it
    }
    EXPLORATION_AGENTS
        .iter()
        .any(|&a| a.eq_ignore_ascii_case(subagent_type))
}

fn derive_project() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}...", &s[..end])
    }
}

fn contains_needs_changes(verdict: &str) -> bool {
    verdict
        .lines()
        .any(|line| line.contains("VERDICT:") && line.contains("needs-changes"))
}

fn deny(reason: &str) {
    let escaped = reason.replace('\\', "\\\\").replace('"', "\\\"");
    println!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Architect: {escaped}"}}}}"#
    );
}
