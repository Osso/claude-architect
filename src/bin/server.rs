use anyhow::{Context, Result};
use claude_architect::{
    Request, Response, build_validation_prompt, socket_path, strip_frontmatter,
};
use peercred_ipc::Server;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio::process::Command;
use tokio::sync::Mutex;

/// Absolute path to the `claude` CLI, resolved once at startup.
static CLAUDE_BIN: OnceLock<PathBuf> = OnceLock::new();

const DESIGN_DOC_INTERVAL: u32 = 20;

const ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "mcp__claude-memory__memory_write",
    "mcp__claude-memory__memory_list",
    "mcp__claude-memory__prompt_search",
    "mcp__claude-memory__answer_search",
];

/// Per-project state: serialization mutex + session tracking.
struct ProjectState {
    mutex: Mutex<SessionInfo>,
}

struct SessionInfo {
    session_id: String,
    created: bool,
    validations: u32,
    /// Buffered completion reports, drained into the next validation prompt.
    pending_reports: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PersistedProject {
    session_id: String,
    validations: u32,
}

struct ServerState {
    projects: Mutex<HashMap<String, Arc<ProjectState>>>,
    data_dir: PathBuf,
    persisted: Mutex<HashMap<String, PersistedProject>>,
}

impl ServerState {
    fn load(data_dir: PathBuf) -> Arc<Self> {
        let sessions_file = data_dir.join("sessions.json");
        let persisted = std::fs::read_to_string(&sessions_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Arc::new(Self {
            projects: Mutex::new(HashMap::new()),
            data_dir,
            persisted: Mutex::new(persisted),
        })
    }

    async fn save(&self) {
        let map = self.persisted.lock().await;
        let path = self.data_dir.join("sessions.json");
        if let Ok(json) = serde_json::to_string_pretty(&*map) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn designs_dir(&self) -> PathBuf {
        self.data_dir.join("designs")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let claude_bin = which::which("claude").context(
        "claude CLI not found in PATH. Ensure ~/.local/bin is in PATH \
         or set Environment=PATH=... in the systemd unit.",
    )?;
    eprintln!("using claude at {}", claude_bin.display());
    CLAUDE_BIN.set(claude_bin).unwrap();

    let path = socket_path();
    let server = Server::bind(&path)?;
    eprintln!("claude-architect listening on {path}");

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp/claude"))
        .join("claude-architect");
    std::fs::create_dir_all(data_dir.join("designs"))?;
    std::fs::create_dir_all(data_dir.join("logs"))?;
    let state = ServerState::load(data_dir);

    loop {
        let (mut conn, _caller) = server.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let request: Request = match conn.read().await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("read error: {e}");
                    return;
                }
            };

            let response = dispatch(state, request).await;

            if let Err(e) = conn.write(&response).await {
                eprintln!("write error: {e}");
            }
        });
    }
}

async fn dispatch(state: Arc<ServerState>, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::Validate {
            project,
            goal,
            tasks,
            cwd,
        } => {
            let ps = get_project_state(&state, &project).await;
            match handle_validate(state, ps, &project, &goal, &tasks, &cwd).await {
                Ok(verdict) => Response::Verdict(verdict),
                Err(e) => Response::Error(format!("{e:#}")),
            }
        }
        Request::Report {
            project,
            task_description,
            assessment,
            cwd,
        } => {
            let ps = get_project_state(&state, &project).await;
            match handle_report(&state, ps, &project, &task_description, &assessment, &cwd)
                .await
            {
                Ok(ack) => Response::Verdict(ack),
                Err(e) => Response::Error(format!("{e:#}")),
            }
        }
        Request::Reset { project, cwd } => {
            let ps = get_project_state(&state, &project).await;
            match handle_reset(state, ps, &project, &cwd).await {
                Ok(msg) => Response::Verdict(msg),
                Err(e) => Response::Error(format!("{e:#}")),
            }
        }
    }
}

async fn get_project_state(
    state: &ServerState,
    project: &str,
) -> Arc<ProjectState> {
    let mut map = state.projects.lock().await;
    map.entry(project.to_string())
        .or_insert_with(|| {
            let persisted = state.persisted.try_lock().ok();
            match persisted.as_ref().and_then(|p| p.get(project)) {
                Some(pp) => Arc::new(ProjectState {
                    mutex: Mutex::new(SessionInfo {
                        session_id: pp.session_id.clone(),
                        created: true,
                        validations: pp.validations,
                        pending_reports: Vec::new(),
                    }),
                }),
                None => Arc::new(ProjectState {
                    mutex: Mutex::new(SessionInfo {
                        session_id: new_uuid(),
                        created: false,
                        validations: 0,
                        pending_reports: Vec::new(),
                    }),
                }),
            }
        })
        .clone()
}

fn new_uuid() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut h1 = DefaultHasher::new();
    SystemTime::now().hash(&mut h1);
    std::process::id().hash(&mut h1);
    let a = h1.finish();
    let mut h2 = DefaultHasher::new();
    a.hash(&mut h2);
    let b = h2.finish();
    // UUIDv4: version nibble = 4, variant bits = 10xx
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16 & 0xFFFF,
        a as u16 & 0x0FFF,
        (b >> 48) as u16 & 0x3FFF | 0x8000,
        b & 0xFFFF_FFFF_FFFF,
    )
}

fn build_prompt_with_reports(goal: &str, tasks: &[String], info: &mut SessionInfo) -> String {
    let mut prompt = build_validation_prompt(goal, tasks);
    if !info.pending_reports.is_empty() {
        prompt.push_str("\n## Completed Since Last Validation\n\n");
        for report in info.pending_reports.drain(..) {
            prompt.push_str(&report);
            prompt.push('\n');
        }
    }
    prompt
}

/// Updates session state after a successful call, returns (should_generate, should_reset,
/// session_id_for_doc) and drops the lock.
async fn apply_validate_result(
    server: &ServerState,
    project: &str,
    info: &mut SessionInfo,
    design_path: &std::path::Path,
    new_session_id: Option<String>,
) -> (bool, bool, String) {
    if let Some(ref new_id) = new_session_id {
        eprintln!("session expired for {project}, reset to {new_id}");
        info.session_id = new_id.clone();
        info.created = true;
    }
    info.validations += 1;
    if !info.created {
        info.created = true;
    }
    persist_project(server, project, &info.session_id, info.validations).await;
    let should_generate = !design_path.exists() || info.validations % DESIGN_DOC_INTERVAL == 0;
    let should_reset = info.validations % DESIGN_DOC_INTERVAL == 0;
    let session_id_for_doc = info.session_id.clone();
    if should_reset {
        append_log(&server.data_dir, project, &LogEntry::SessionReset { timestamp: now_utc() });
        info.session_id = new_uuid();
        info.created = false;
        info.validations = 0;
        info.pending_reports.clear();
        persist_project(server, project, &info.session_id, 0).await;
    }
    (should_generate, should_reset, session_id_for_doc)
}

async fn handle_validate(
    server: Arc<ServerState>,
    ps: Arc<ProjectState>,
    project: &str,
    goal: &str,
    tasks: &[String],
    cwd: &str,
) -> Result<String> {
    let mut info = ps.mutex.lock().await;
    let prompt = build_prompt_with_reports(goal, tasks, &mut info);
    let design_path = server.designs_dir().join(format!("{project}.md"));

    let result = call_claude(&prompt, &info.session_id, info.created, &design_path, cwd, 120).await?;

    let ts = now_utc();
    append_log(&server.data_dir, project, &LogEntry::User { text: prompt.clone(), timestamp: ts.clone() });
    append_log(&server.data_dir, project, &LogEntry::Assistant {
        text: result.output.text.clone(),
        timestamp: ts,
        usage: result.output.usage.clone(),
    });

    let (should_generate, _, session_id_for_doc) =
        apply_validate_result(&server, project, &mut info, &design_path, result.new_session_id).await;
    let project_owned = project.to_string();
    let cwd_owned = cwd.to_string();
    drop(info);

    if should_generate {
        tokio::spawn(async move {
            request_design_doc(server, session_id_for_doc, project_owned, cwd_owned).await;
        });
    }

    Ok(result.output.text)
}

async fn handle_reset(
    server: Arc<ServerState>,
    ps: Arc<ProjectState>,
    project: &str,
    cwd: &str,
) -> Result<String> {
    let mut info = ps.mutex.lock().await;

    let session_id_for_doc = info.session_id.clone();
    let was_created = info.created;

    append_log(&server.data_dir, project, &LogEntry::SessionReset { timestamp: now_utc() });
    info.session_id = new_uuid();
    info.created = false;
    info.validations = 0;
    info.pending_reports.clear();
    persist_project(&server, project, &info.session_id, 0).await;

    drop(info);

    if was_created {
        // Reset is user-triggered and infrequent; await the doc generation directly.
        request_design_doc(server, session_id_for_doc, project.to_string(), cwd.to_string()).await;
    }

    Ok(format!("Session reset for {project}. Design doc regenerated."))
}

async fn handle_report(
    _server: &ServerState,
    ps: Arc<ProjectState>,
    _project: &str,
    task_description: &str,
    assessment: &str,
    _cwd: &str,
) -> Result<String> {
    let mut info = ps.mutex.lock().await;
    let report = format!("- {task_description}: {assessment}");
    info.pending_reports.push(report);
    Ok("buffered".to_string())
}

async fn persist_project(
    server: &ServerState,
    project: &str,
    session_id: &str,
    validations: u32,
) {
    let mut persisted = server.persisted.lock().await;
    persisted.insert(
        project.to_string(),
        PersistedProject {
            session_id: session_id.to_string(),
            validations,
        },
    );
    drop(persisted);
    server.save().await;
}

async fn request_design_doc(
    server: Arc<ServerState>,
    session_id: String,
    project: String,
    cwd: String,
) {
    let prompt = "Summarize your current understanding of this project's \
        architecture into a concise design document. Cover: key modules, \
        ownership boundaries, recurring patterns, known constraints, and \
        common task decomposition pitfalls. Output ONLY the document content, \
        no preamble.";

    let design_path = server.designs_dir().join(format!("{project}.md"));
    match call_claude(prompt, &session_id, true, &design_path, &cwd, 300).await {
        Ok(result) => {
            if let Err(e) = std::fs::write(&design_path, &result.output.text) {
                eprintln!("failed to write design doc: {e}");
            } else {
                eprintln!("updated design doc for {project}");
            }
        }
        Err(e) => eprintln!("design doc request failed: {e:#}"),
    }
}

struct ClaudeOutput {
    text: String,
    usage: Option<LogUsage>,
}

#[derive(Serialize, Clone)]
struct LogUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum LogEntry {
    #[serde(rename = "user")]
    User { text: String, timestamp: String },
    #[serde(rename = "assistant")]
    Assistant { text: String, timestamp: String, usage: Option<LogUsage> },
    #[serde(rename = "session_reset")]
    SessionReset { timestamp: String },
}

struct CallResult {
    output: ClaudeOutput,
    /// Set when a stale session was detected and a fresh one was created.
    new_session_id: Option<String>,
}

async fn call_claude(
    prompt: &str,
    session_id: &str,
    resume: bool,
    design_path: &std::path::Path,
    cwd: &str,
    timeout_secs: u64,
) -> Result<CallResult> {
    match run_claude(prompt, session_id, resume, design_path, cwd, timeout_secs).await {
        Ok(output) => Ok(CallResult { output, new_session_id: None }),
        Err(e) if resume && format!("{e:#}").contains("No conversation found") => {
            eprintln!("session {session_id} expired, starting fresh");
            let new_id = new_uuid();
            let output = run_claude(prompt, &new_id, false, design_path, cwd, timeout_secs).await?;
            Ok(CallResult { output, new_session_id: Some(new_id) })
        }
        Err(e) => Err(e),
    }
}

fn append_log(data_dir: &std::path::Path, project: &str, entry: &LogEntry) {
    let logs_dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let path = logs_dir.join(format!("{project}.jsonl"));
    if let Ok(json) = serde_json::to_string(entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{json}");
        }
    }
}

fn now_utc() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    let (y, m, day) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    // Howard Hinnant's algorithm
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

fn build_claude_command(
    prompt: &str,
    session_id: &str,
    resume: bool,
    design_path: &std::path::Path,
    cwd: &str,
) -> Result<Command> {
    let mut cmd = Command::new(CLAUDE_BIN.get().unwrap());
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--model")
        .arg("opus")
        .arg("--effort")
        .arg("low")
        .arg("--permission-mode")
        .arg("dontAsk")
        .arg("--allowedTools")
        .arg(ALLOWED_TOOLS.join(","));
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    cmd.env_remove("CLAUDECODE");
    if resume {
        cmd.arg("--resume").arg(session_id);
    } else {
        let mut system = load_architect_prompt()?;
        system.push_str(&format!(
            "\n\n## Design Document\n\nRead the design document at `{}` \
             before validating. If the file doesn't exist yet, proceed without it.",
            design_path.display()
        ));
        cmd.arg("--session-id").arg(session_id);
        cmd.arg("--system-prompt").arg(system);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(cmd)
}

fn parse_claude_output(stdout: &[u8]) -> ClaudeOutput {
    let text_raw = String::from_utf8_lossy(stdout);

    // Output is a JSON array — find the "result" entry
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&text_raw) {
        if let Some(result_entry) = arr.iter().find(|v| v.get("type").and_then(|t| t.as_str()) == Some("result")) {
            let text = result_entry.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string();
            let usage = result_entry.get("usage").map(|u| LogUsage {
                input: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                output: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_read: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            });
            return ClaudeOutput { text, usage };
        }
    }

    // Fallback: try as single object (older format)
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text_raw) {
        if let Some(result) = val.get("result").and_then(|r| r.as_str()) {
            let usage = val.get("usage").map(|u| LogUsage {
                input: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                output: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_read: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            });
            return ClaudeOutput { text: result.to_string(), usage };
        }
    }

    ClaudeOutput { text: text_raw.to_string(), usage: None }
}

async fn run_claude(
    prompt: &str,
    session_id: &str,
    resume: bool,
    design_path: &std::path::Path,
    cwd: &str,
    timeout_secs: u64,
) -> Result<ClaudeOutput> {
    let mut cmd = build_claude_command(prompt, session_id, resume, design_path, cwd)?;
    let child = cmd.spawn().context("spawn claude")?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    .context(format!("claude CLI timed out after {}s", timeout_secs))?
    .context("spawn claude")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude exited {}: {stderr}", output.status);
    }

    Ok(parse_claude_output(&output.stdout))
}

fn load_architect_prompt() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = format!("{home}/.claude/agents/architect.md");
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    Ok(strip_frontmatter(&content).to_string())
}
