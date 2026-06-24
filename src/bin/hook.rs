#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use claude_architect::{
    Request, Response, build_assessment_prompt, contains_incomplete, contains_needs_changes,
    deny_json, feedback_json, should_skip, socket_path, truncate,
};
use peercred_ipc::{Client, IpcError};
use std::io::Read;
use std::time::Duration;

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
    let tool_input = task_tool_input(json)?;
    let description = string_field(tool_input, "description");
    let prompt = string_field(tool_input, "prompt");
    let (cwd, project) = current_project_context();
    let summary = summarize_task(description, prompt);

    Some(Request::Validate {
        project,
        goal: description.to_string(),
        tasks: vec![summary],
        cwd,
    })
}

fn validate_and_respond(request: Request) {
    let path = socket_path();
    let response = match Client::call_timeout::<_, Request, Response>(
        &path,
        &request,
        Duration::from_secs(180),
    ) {
        Ok(r) => r,
        Err(IpcError::Timeout(_)) => {
            eprintln!("claude-architect-hook: architect validation timed out, allowing");
            return;
        }
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

fn try_post_tool_use(json: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    let Some(tool_input) = task_tool_input_result(json)? else {
        return Ok(());
    };

    let description = string_field_or(tool_input, "description", "unknown task");
    let tool_response = extract_response_text(json);
    if tool_response.is_empty() {
        return Ok(());
    }

    let assessment = assess_task_completion(description, &tool_response)?;
    let request = build_report_request(description, assessment.clone());
    send_report_async(request);

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

fn task_tool_input<'a>(json: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
    let tool_name = json.get("tool_name")?.as_str()?;
    if tool_name != "Task" {
        return None;
    }

    let tool_input = json.get("tool_input")?;
    if should_skip(string_field(tool_input, "subagent_type")) {
        return None;
    }

    Some(tool_input)
}

fn task_tool_input_result<'a>(
    json: &'a serde_json::Value,
) -> Result<Option<&'a serde_json::Value>, Box<dyn std::error::Error>> {
    let tool_name = string_field(json, "tool_name");
    if tool_name != "Task" {
        return Ok(None);
    }

    let tool_input = json.get("tool_input").ok_or("no tool_input")?;
    if should_skip(string_field(tool_input, "subagent_type")) {
        return Ok(None);
    }

    Ok(Some(tool_input))
}

fn string_field<'a>(json: &'a serde_json::Value, key: &str) -> &'a str {
    json.get(key).and_then(|value| value.as_str()).unwrap_or("")
}

fn string_field_or<'a>(json: &'a serde_json::Value, key: &str, default: &'a str) -> &'a str {
    json.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
}

fn current_project_context() -> (String, String) {
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = derive_project(&cwd);
    (cwd, project)
}

fn summarize_task(description: &str, prompt: &str) -> String {
    if prompt.is_empty() {
        return description.to_string();
    }

    // Send first 200 chars of prompt as context, not the full detailed prompt.
    // The architect only needs enough to check for conflicts/gaps.
    truncate(prompt, 200)
}

fn assess_task_completion(
    description: &str,
    tool_response: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let truncated = truncate(tool_response, 4000);
    let prompt = build_assessment_prompt(description, &truncated);
    call_haiku(&prompt)
}

fn build_report_request(description: &str, assessment: String) -> Request {
    let (cwd, project) = current_project_context();
    Request::Report {
        project,
        task_description: description.to_string(),
        assessment,
        cwd,
    }
}

fn send_report_async(request: Request) {
    // Fire-and-forget: send report to daemon in a background thread.
    std::thread::spawn(move || {
        let path = socket_path();
        if let Err(e) =
            Client::call_timeout::<_, Request, Response>(&path, &request, Duration::from_secs(30))
        {
            if !matches!(e, IpcError::Timeout(_)) {
                eprintln!("claude-architect-hook: report failed: {e}");
            }
        }
    });
}
