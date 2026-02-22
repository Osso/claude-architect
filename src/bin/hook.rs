use claude_architect::{
    Request, Response, build_assessment_prompt, contains_incomplete, contains_needs_changes,
    deny_json, feedback_json, should_skip, socket_path, truncate,
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

    let event = json
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("PreToolUse");

    match event {
        "PreToolUse" => handle_pre_tool_use(&json),
        "PostToolUse" => handle_post_tool_use(&json),
        _ => {}
    }
}

// --- PreToolUse (existing logic) ---

fn handle_pre_tool_use(json: &serde_json::Value) {
    if let Some(request) = extract_validate_request(json) {
        validate_and_respond(request);
    }
}

fn extract_validate_request(json: &serde_json::Value) -> Option<Request> {
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

    // Send first 200 chars of prompt as context, not the full detailed prompt.
    // The architect only needs enough to check for conflicts/gaps.
    let summary = if prompt.is_empty() {
        description.to_string()
    } else {
        truncate(prompt, 200)
    };

    Some(Request::Validate {
        project,
        goal: description.to_string(),
        tasks: vec![summary],
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

// --- PostToolUse (task completion reporting) ---

fn handle_post_tool_use(json: &serde_json::Value) {
    if let Err(e) = try_post_tool_use(json) {
        eprintln!("claude-architect-hook: post-tool-use: {e}");
    }
}

fn try_post_tool_use(
    json: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let tool_name = json
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tool_name != "Task" {
        return Ok(());
    }

    let tool_input = json.get("tool_input").ok_or("no tool_input")?;
    let subagent_type = tool_input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if should_skip(subagent_type) {
        return Ok(());
    }

    let description = tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown task");

    let tool_response = extract_response_text(json);
    if tool_response.is_empty() {
        return Ok(());
    }

    let truncated = truncate(&tool_response, 4000);
    let prompt = build_assessment_prompt(description, &truncated);

    let assessment = call_haiku(&prompt)?;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = derive_project(&cwd);

    let request = Request::Report {
        project,
        task_description: description.to_string(),
        assessment: assessment.clone(),
        cwd,
    };

    // Fire-and-forget: send report to daemon in a background thread.
    std::thread::spawn(move || {
        let path = socket_path();
        if let Err(e) = Client::call::<_, Request, Response>(&path, &request) {
            eprintln!("claude-architect-hook: report failed: {e}");
        }
    });

    if contains_incomplete(&assessment) {
        println!("{}", feedback_json(&assessment));
    }

    Ok(())
}

fn extract_response_text(json: &serde_json::Value) -> String {
    match json.get("tool_response") {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn call_haiku(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("claude")
        .arg("-p")
        .arg(prompt)
        .arg("--model")
        .arg("haiku")
        .env_remove("CLAUDECODE")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude haiku exited {}: {stderr}", output.status).into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// --- Shared ---

fn derive_project(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}
