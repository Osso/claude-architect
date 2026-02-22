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
        Err(_) => return,
    };

    if let Some(request) = extract_request(&json) {
        validate_and_respond(request);
    }
}

fn extract_request(json: &serde_json::Value) -> Option<Request> {
    let tool_name = json.get("tool_name")?.as_str()?;
    if tool_name != "Task" {
        return None;
    }

    let tool_input = json.get("tool_input")?;
    let subagent_type = tool_input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if should_skip(subagent_type) {
        return None;
    }

    let description = tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = tool_input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = derive_project(&cwd);

    Some(Request::Validate {
        project,
        goal: description.to_string(),
        tasks: vec![truncate(prompt, 2000)],
        cwd,
    })
}

fn validate_and_respond(request: Request) {
    let path = socket_path();
    let response = match Client::call::<_, Request, Response>(&path, &request) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("claude-architect-hook: service error: {e}");
            return;
        }
    };

    match response {
        Response::Verdict(verdict) => {
            if contains_needs_changes(&verdict) {
                println!("{}", deny_json(&verdict));
            }
        }
        Response::Error(e) => {
            eprintln!("claude-architect-hook: architect error: {e}");
        }
        Response::Pong => {}
    }
}

fn derive_project(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}
