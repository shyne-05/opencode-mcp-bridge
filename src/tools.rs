use crate::{
    backend::{Backend, PromptRequest, resolve_directory},
    browser::run_browser_action,
    process::{native_shell_name, run_shell},
    state::{AppState, Principal},
    util::{optional_string_arg, required_string_arg},
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::RequestContext,
};
use serde_json::{Map, Value, json};
use std::{borrow::Cow, sync::Arc, time::Instant};

#[derive(Clone)]
pub struct BridgeServer {
    state: AppState,
}

impl BridgeServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn tool_list(&self) -> Vec<Tool> {
        let mut tools = vec![
            tool(
                "bridge_prompt",
                "Send a prompt to the configured local agent backend.",
                json!({
                    "prompt": {"type": "string"},
                    "sessionId": {"type": "string"},
                    "agent": {"type": "string"},
                    "model": {"type": "string"},
                    "directory": {"type": "string"}
                }),
                &["prompt"],
                ToolAnnotations::new().read_only(false).open_world(true),
            ),
            tool(
                "bridge_read_file",
                "Read an existing file inside BRIDGE_WORKDIR through the configured backend.",
                json!({"path": {"type": "string"}}),
                &["path"],
                ToolAnnotations::new().read_only(true).open_world(false),
            ),
            tool(
                "bridge_search",
                "Search the configured backend workspace.",
                json!({"pattern": {"type": "string"}}),
                &["pattern"],
                ToolAnnotations::new().read_only(true).open_world(false),
            ),
        ];

        if self.state.config.tools.shell {
            tools.push(tool(
                "shell",
                "Run an unrestricted native shell command on the host (Bash on Linux, Zsh on macOS, PowerShell on Windows) with a sanitized child environment, bounded output, timeout, process-tree cleanup, and concurrency control. Intended for a trusted personal workstation.",
                json!({
                    "command": {"type": "string"},
                    "directory": {"type": "string"}
                }),
                &["command"],
                ToolAnnotations::new()
                    .read_only(false)
                    .destructive(true)
                    .open_world(true),
            ));
        }
        if self.state.config.tools.browser {
            tools.push(tool(
                "browser",
                "Control a local Chrome debugging session. Browser cookies and page data are available to this tool.",
                json!({
                    "action": {"type": "string", "enum": ["navigate", "tabs", "close", "evaluate", "new", "snapshot", "click", "fill"]},
                    "url": {"type": "string"},
                    "targetId": {"type": "string"},
                    "expression": {"type": "string"},
                    "selector": {"type": "string"},
                    "value": {"type": "string"}
                }),
                &["action"],
                ToolAnnotations::new().read_only(false).open_world(true),
            ));
        }
        tools
    }

    async fn dispatch(
        &self,
        principal: &str,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<String, String> {
        let backend = Backend::new(&self.state);
        match name {
            "bridge_read_file" => backend.read_file(required_string_arg(args, "path")?).await,
            "bridge_search" => backend.search(required_string_arg(args, "pattern")?).await,
            "bridge_prompt" => {
                backend
                    .prompt(
                        principal,
                        PromptRequest {
                            prompt: required_string_arg(args, "prompt")?,
                            session_id: optional_string_arg(args, "sessionId"),
                            directory: optional_string_arg(args, "directory"),
                            agent: optional_string_arg(args, "agent"),
                            model: optional_string_arg(args, "model"),
                        },
                    )
                    .await
            }
            "shell" if self.state.config.tools.shell => {
                let queued_at = Instant::now();
                let permit = self
                    .state
                    .shell_slots
                    .acquire()
                    .await
                    .map_err(|_| "shell concurrency limiter closed".to_string())?;
                let queue_ms = queued_at.elapsed().as_secs_f64() * 1_000.0;
                let directory = resolve_directory(
                    &self.state.config.workdir,
                    optional_string_arg(args, "directory"),
                )?;
                let started_at = Instant::now();
                let output = run_shell(
                    required_string_arg(args, "command")?,
                    &directory,
                    &self.state.config.process,
                )
                .await;
                let success = output.is_success();
                tracing::info!(
                    target: "mcp_bridge::latency",
                    tool = "shell",
                    shell = native_shell_name(),
                    queue_ms,
                    elapsed_ms = started_at.elapsed().as_secs_f64() * 1_000.0,
                    success,
                    "tool execution latency"
                );
                drop(permit);
                let rendered = format!(
                    "shell:{}\ndir:{}\n{}",
                    native_shell_name(),
                    directory.display(),
                    output.render()
                );
                if success { Ok(rendered) } else { Err(rendered) }
            }
            "browser" if self.state.config.tools.browser => {
                let action = optional_string_arg(args, "action").unwrap_or("tabs");
                run_browser_action(&self.state, action, args).await
            }
            _ => Err(format!("unknown or disabled tool: {name}")),
        }
    }
}

impl ServerHandler for BridgeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mcp-bridge", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Personal desktop automation gateway. Host tools are powerful and are exposed only when explicitly enabled by server configuration.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tool_list()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_list().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let principal = principal_from_context(&context)?;
        let tool_name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        let started_at = Instant::now();
        let dispatched = self.dispatch(&principal.0, &tool_name, &args).await;
        let success = dispatched.is_ok();
        tracing::info!(
            target: "mcp_bridge::latency",
            tool = %tool_name,
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1_000.0,
            success,
            "mcp tool latency"
        );
        let result = match dispatched {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        };
        Ok(result.into())
    }
}

fn principal_from_context(context: &RequestContext<RoleServer>) -> Result<Principal, McpError> {
    let parts = context
        .extensions
        .get::<axum::http::request::Parts>()
        .ok_or_else(|| McpError::internal_error("HTTP request context is unavailable", None))?;
    parts
        .extensions
        .get::<Principal>()
        .cloned()
        .ok_or_else(|| McpError::internal_error("authenticated principal is unavailable", None))
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
    annotations: ToolAnnotations,
) -> Tool {
    let properties = properties.as_object().cloned().unwrap_or_default();
    let schema: JsonObject = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    Tool::new(
        Cow::Borrowed(name),
        Cow::Borrowed(description),
        Arc::new(schema),
    )
    .with_annotations(annotations)
}
