use anyhow::Result;
use claude_architect::{Request, Response, socket_path};
use peercred_ipc::Client;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone)]
struct ArchitectMcp {
    tool_router: ToolRouter<Self>,
}

impl ArchitectMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ValidateParams {
    /// Project name for memory context (e.g., "globalcomix", "sakuin")
    project: String,

    /// High-level goal of the task decomposition
    goal: String,

    /// Short task descriptions to validate (1-2 sentences each)
    tasks: Vec<String>,

    /// Working directory of the project (defaults to current directory)
    cwd: Option<String>,
}

#[tool_router]
impl ArchitectMcp {
    #[tool(
        description = "Validate a task decomposition before spawning workers. Checks for conflicts, gaps, ordering issues, and scope problems. Returns a structured verdict."
    )]
    async fn architect_validate(
        &self,
        Parameters(params): Parameters<ValidateParams>,
    ) -> String {
        let cwd = params.cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        let request = Request::Validate {
            project: params.project,
            goal: params.goal,
            tasks: params.tasks,
            cwd,
        };

        let path = socket_path();
        match tokio::task::spawn_blocking(move || {
            Client::call::<_, Request, Response>(&path, &request)
        })
        .await
        {
            Ok(Ok(Response::Verdict(v))) => v,
            Ok(Ok(Response::Error(e))) => format!("Architect error: {e}"),
            Ok(Ok(Response::Pong)) => "Unexpected pong response".to_string(),
            Ok(Err(e)) => format!("IPC error (is claude-architect service running?): {e}"),
            Err(e) => format!("Task join error: {e}"),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ArchitectMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Claude Architect service — validates task decompositions before spawning worker agents. \
                 Call architect_validate with a project name, goal, and list of task descriptions."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let service = ArchitectMcp::new();
    let server = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
