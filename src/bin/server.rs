use anyhow::{Context, Result};
use claude_architect::{Request, Response, socket_path};
use peercred_ipc::Server;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, Filter, PointStruct, ScrollPointsBuilder, UpsertPointsBuilder,
};
use std::process::Stdio;
use tokio::process::Command;

const QDRANT_URL: &str = "http://localhost:6334";
const COLLECTION: &str = "claude-memory";
const VECTOR_SIZE: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let path = socket_path();
    let server = Server::bind(&path)?;
    eprintln!("claude-architect listening on {path}");

    loop {
        let (mut conn, _caller) = server.accept().await?;
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
                } => match handle_validate(&project, &goal, &tasks).await {
                    Ok(verdict) => Response::Verdict(verdict),
                    Err(e) => Response::Error(format!("{e:#}")),
                },
            };

            if let Err(e) = conn.write(&response).await {
                eprintln!("write error: {e}");
            }
        });
    }
}

async fn handle_validate(project: &str, goal: &str, tasks: &[String]) -> Result<String> {
    let memories = load_memories(project).await;
    let instructions = load_architect_prompt()?;
    let prompt = build_prompt(&instructions, &memories, goal, tasks);
    let response = call_claude(&prompt).await?;
    extract_and_store_memories(&response, project).await;
    Ok(response)
}

async fn load_memories(project: &str) -> Vec<String> {
    match try_load_memories(project).await {
        Ok(mems) => mems,
        Err(e) => {
            eprintln!("warning: failed to load memories: {e:#}");
            Vec::new()
        }
    }
}

async fn try_load_memories(project: &str) -> Result<Vec<String>> {
    let client = Qdrant::from_url(QDRANT_URL)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("qdrant connect")?;

    let mut conditions = vec![Condition::matches(
        "category",
        "architect".to_string(),
    )];
    if !project.is_empty() {
        conditions.push(Condition::matches("project", project.to_string()));
    }
    let filter = Filter::must(conditions);

    let mut entries = Vec::new();
    let mut offset = None;

    loop {
        let mut scroll = ScrollPointsBuilder::new(COLLECTION)
            .limit(100)
            .with_payload(true)
            .filter(filter.clone());
        if let Some(off) = offset {
            scroll = scroll.offset(off);
        }

        let result = client.scroll(scroll).await.context("scroll")?;
        for point in &result.result {
            if let Some(text) = get_payload_string(&point.payload, "text") {
                entries.push(text);
            }
        }

        offset = result.next_page_offset;
        if offset.is_none() {
            break;
        }
    }

    Ok(entries)
}

fn get_payload_string(
    payload: &std::collections::HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    payload
        .get(key)
        .and_then(|v| v.kind.as_ref())
        .and_then(|k| match k {
            qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
            _ => None,
        })
}

fn load_architect_prompt() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = format!("{home}/.claude/agents/architect.md");
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;

    // Strip YAML frontmatter
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            return Ok(content[end + 6..].trim_start().to_string());
        }
    }
    Ok(content)
}

fn build_prompt(
    instructions: &str,
    memories: &[String],
    goal: &str,
    tasks: &[String],
) -> String {
    let mut prompt = String::new();
    prompt.push_str(instructions);
    prompt.push_str("\n\n");

    if !memories.is_empty() {
        prompt.push_str("## Prior Knowledge\n\n");
        for mem in memories {
            prompt.push_str("- ");
            prompt.push_str(mem);
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    prompt.push_str("## Goal\n\n");
    prompt.push_str(goal);
    prompt.push_str("\n\n## Tasks to Validate\n\n");
    for (i, task) in tasks.iter().enumerate() {
        prompt.push_str(&format!("{}. {}\n", i + 1, task));
    }

    prompt
}

async fn call_claude(prompt: &str) -> Result<String> {
    let output = Command::new("claude")
        .args(["-p", prompt, "--model", "opus"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("spawn claude")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude exited {}: {stderr}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn extract_and_store_memories(response: &str, project: &str) {
    let memories = parse_remember_section(response);
    if memories.is_empty() {
        return;
    }
    if let Err(e) = store_memories(&memories, project).await {
        eprintln!("failed to store memories: {e:#}");
    }
}

fn parse_remember_section(response: &str) -> Vec<String> {
    let mut memories = Vec::new();
    let mut in_remember = false;

    for line in response.lines() {
        if line.trim() == "REMEMBER:" {
            in_remember = true;
            continue;
        }
        if in_remember {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || (!trimmed.starts_with('-') && !trimmed.starts_with('*'))
            {
                break;
            }
            let text = trimmed
                .trim_start_matches(|c| c == '-' || c == '*')
                .trim();
            if !text.is_empty() {
                memories.push(text.to_string());
            }
        }
    }

    memories
}

async fn store_memories(memories: &[String], project: &str) -> Result<()> {
    let client = Qdrant::from_url(QDRANT_URL)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("qdrant connect")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let zero_vec = vec![0.0f32; VECTOR_SIZE];

    for (i, text) in memories.iter().enumerate() {
        let point = PointStruct::new(
            now + i as u64,
            zero_vec.clone(),
            [
                ("text", text.clone().into()),
                ("source", "architect-service".to_string().into()),
                ("category", "architect".to_string().into()),
                ("project", project.to_string().into()),
                ("hash", String::new().into()),
            ],
        );
        client
            .upsert_points(UpsertPointsBuilder::new(COLLECTION, vec![point]))
            .await
            .context("upsert")?;
    }

    Ok(())
}
