use claude_architect::{
    Request, Response, contains_needs_changes, deny_json, should_skip, socket_path,
    truncate,
};
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
                println!("{}", deny_json(&verdict));
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

fn derive_project() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default()
}
