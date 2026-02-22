use anyhow::{Context, Result};
use claude_architect::{
    Request, Response, build_validation_prompt, socket_path, strip_frontmatter,
};
use peercred_ipc::Server;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

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
    let path = socket_path();
    let server = Server::bind(&path)?;
    eprintln!("claude-architect listening on {path}");

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp/claude"))
        .join("claude-architect");
    std::fs::create_dir_all(data_dir.join("designs"))?;
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

            let response = match request {
                Request::Ping => Response::Pong,
                Request::Validate {
                    project,
                    goal,
                    tasks,
                    cwd,
                } => {
                    let ps = get_project_state(&state, &project).await;
                    match handle_validate(&state, ps, &project, &goal, &tasks, &cwd)
                        .await
                    {
                        Ok(verdict) => Response::Verdict(verdict),
                        Err(e) => Response::Error(format!("{e:#}")),
                    }
                }
            };

            if let Err(e) = conn.write(&response).await {
                eprintln!("write error: {e}");
            }
        });
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
                    }),
                }),
                None => Arc::new(ProjectState {
                    mutex: Mutex::new(SessionInfo {
                        session_id: new_uuid(),
                        created: false,
                        validations: 0,
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

async fn handle_validate(
    server: &ServerState,
    ps: Arc<ProjectState>,
    project: &str,
    goal: &str,
    tasks: &[String],
    cwd: &str,
) -> Result<String> {
    let mut info = ps.mutex.lock().await;

    let prompt = build_validation_prompt(goal, tasks);
    let design_path = server.designs_dir().join(format!("{project}.md"));

    let response = call_claude(
        &prompt,
        &info.session_id,
        info.created,
        &design_path,
        cwd,
    )
    .await?;

    info.validations += 1;
    if !info.created {
        info.created = true;
    }

    persist_project(server, project, &info.session_id, info.validations).await;

    // Generate design doc if it doesn't exist yet (first validation ever),
    // or every N validations (reset session to prevent auto-compaction).
    let should_generate = !design_path.exists() || info.validations % DESIGN_DOC_INTERVAL == 0;
    if should_generate {
        request_design_doc(server, &info.session_id, project, cwd).await;
    }
    if info.validations % DESIGN_DOC_INTERVAL == 0 {
        info.session_id = new_uuid();
        info.created = false;
        info.validations = 0;
        persist_project(server, project, &info.session_id, 0).await;
    }

    Ok(response)
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
    server: &ServerState,
    session_id: &str,
    project: &str,
    cwd: &str,
) {
    let prompt = "Summarize your current understanding of this project's \
        architecture into a concise design document. Cover: key modules, \
        ownership boundaries, recurring patterns, known constraints, and \
        common task decomposition pitfalls. Output ONLY the document content, \
        no preamble.";

    let design_path = server.designs_dir().join(format!("{project}.md"));
    match call_claude(prompt, session_id, true, &design_path, cwd).await {
        Ok(doc) => {
            if let Err(e) = std::fs::write(&design_path, &doc) {
                eprintln!("failed to write design doc: {e}");
            } else {
                eprintln!("updated design doc for {project}");
            }
        }
        Err(e) => eprintln!("design doc request failed: {e:#}"),
    }
}

async fn call_claude(
    prompt: &str,
    session_id: &str,
    resume: bool,
    design_path: &std::path::Path,
    cwd: &str,
) -> Result<String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(prompt)
        .arg("--model")
        .arg("opus")
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

    let output = cmd.output().await.context("spawn claude")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude exited {}: {stderr}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn load_architect_prompt() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = format!("{home}/.claude/agents/architect.md");
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    Ok(strip_frontmatter(&content).to_string())
}
