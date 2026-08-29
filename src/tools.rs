use crate::{
    backend::{Backend, PromptRequest, resolve_directory},
    browser::run_browser_action,
    config::AgentKind,
    desktop,
    process::{run_bash, run_program},
    state::{AppState, Principal},
    util::{optional_string_arg, required_string_arg, trunc},
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
use std::{borrow::Cow, sync::Arc};

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
                "bridge_prompt_async",
                "Send a prompt to the configured local agent backend without waiting for completion.",
                json!({
                    "prompt": {"type": "string"},
                    "sessionId": {"type": "string"},
                    "directory": {"type": "string"}
                }),
                &["prompt"],
                ToolAnnotations::new().read_only(false).open_world(true),
            ),
            tool(
                "bridge_session_messages",
                "Read messages from a backend session created by this authenticated principal.",
                json!({
                    "sessionId": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                }),
                &["sessionId"],
                ToolAnnotations::new().read_only(true).open_world(false),
            ),
            tool(
                "bridge_session_status",
                "Read status for a backend session created by this authenticated principal.",
                json!({"sessionId": {"type": "string"}}),
                &["sessionId"],
                ToolAnnotations::new().read_only(true).open_world(false),
            ),
            tool(
                "bridge_list_sessions",
                "List backend sessions owned by this authenticated principal.",
                json!({}),
                &[],
                ToolAnnotations::new().read_only(true).open_world(false),
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
                "Run an unrestricted bash command on the host with a sanitized child environment, bounded output, timeout, and concurrency control. Intended for a trusted personal workstation.",
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
        if self.state.config.tools.agent {
            tools.push(tool(
                "bridge_agent_prompt",
                "Run the configured Codex or OpenCode CLI adapter. Codex enforces the requested sandbox; OpenCode uses its native permission system and enables --auto only for danger-full-access.",
                json!({
                    "prompt": {"type": "string"},
                    "directory": {"type": "string"},
                    "sandbox": {"type": "string", "enum": ["read-only", "workspace-write", "danger-full-access"]}
                }),
                &["prompt"],
                ToolAnnotations::new().read_only(false).open_world(true),
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
        if self.state.config.tools.desktop {
            tools.push(tool(
                "desktop_open_app",
                "Open a desktop application safely without shell-string interpolation. Resolves Flatpak apps, desktop launchers, and executables.",
                json!({"app": {"type": "string"}}),
                &["app"],
                ToolAnnotations::new().read_only(false).open_world(false),
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
            "bridge_list_sessions" => backend.list_sessions(principal).await,
            "bridge_session_messages" => {
                backend
                    .session_messages(
                        principal,
                        required_string_arg(args, "sessionId")?,
                        args.get("limit").and_then(Value::as_u64).unwrap_or(10),
                    )
                    .await
            }
            "bridge_session_status" => {
                backend
                    .session_status(principal, required_string_arg(args, "sessionId")?)
                    .await
            }
            "bridge_prompt" | "bridge_prompt_async" => {
                backend
                    .prompt(
                        principal,
                        PromptRequest {
                            prompt: required_string_arg(args, "prompt")?,
                            session_id: optional_string_arg(args, "sessionId"),
                            directory: optional_string_arg(args, "directory"),
                            agent: optional_string_arg(args, "agent"),
                            model: optional_string_arg(args, "model"),
                            asynchronous: name == "bridge_prompt_async",
                        },
                    )
                    .await
            }
            "shell" if self.state.config.tools.shell => {
                let _permit = self
                    .state
                    .shell_slots
                    .acquire()
                    .await
                    .map_err(|_| "shell concurrency limiter closed".to_string())?;
                let directory = resolve_directory(
                    &self.state.config.workdir,
                    optional_string_arg(args, "directory"),
                )?;
                let output = run_bash(
                    required_string_arg(args, "command")?,
                    &directory,
                    &self.state.config.process,
                )
                .await;
                let rendered = format!("dir:{}\n{}", directory.display(), output.render());
                if output.is_success() {
                    Ok(rendered)
                } else {
                    Err(rendered)
                }
            }
            "bridge_agent_prompt" if self.state.config.tools.agent => {
                let _permit = self
                    .state
                    .agent_slots
                    .acquire()
                    .await
                    .map_err(|_| "agent concurrency limiter closed".to_string())?;
                let prompt = required_string_arg(args, "prompt")?;
                let sandbox = optional_string_arg(args, "sandbox").unwrap_or("read-only");
                if !matches!(
                    sandbox,
                    "read-only" | "workspace-write" | "danger-full-access"
                ) {
                    return Err(
                        "sandbox must be read-only, workspace-write, or danger-full-access"
                            .to_string(),
                    );
                }
                let command = self.state.config.agent_command.as_deref().ok_or_else(|| {
                    "MCP_AGENT_COMMAND is required for bridge_agent_prompt".to_string()
                })?;
                let directory = resolve_directory(
                    &self.state.config.workdir,
                    optional_string_arg(args, "directory"),
                )?;
                let kind = self.state.config.agent_kind.ok_or_else(||
                    "MCP_AGENT_KIND=codex or MCP_AGENT_KIND=opencode is required for bridge_agent_prompt".to_string())?;
                let args = agent_arguments(kind, sandbox, &directory, prompt);
                let output = run_program(
                    command,
                    &args,
                    Some(&directory),
                    self.state.config.process.agent_timeout,
                    &self.state.config.process,
                )
                .await;
                let rendered = trunc(&output.render(), 25_000);
                if output.is_success() {
                    Ok(rendered)
                } else {
                    Err(rendered)
                }
            }
            "browser" if self.state.config.tools.browser => {
                let action = optional_string_arg(args, "action").unwrap_or("tabs");
                run_browser_action(&self.state, action, args).await
            }
            "desktop_open_app" if self.state.config.tools.desktop => {
                desktop::open_app(&self.state, required_string_arg(args, "app")?).await
            }
            _ => Err(format!("unknown or disabled tool: {name}")),
        }
    }
}

fn agent_arguments(
    kind: AgentKind,
    sandbox: &str,
    directory: &std::path::Path,
    prompt: &str,
) -> Vec<String> {
    match kind {
        AgentKind::Codex => vec![
            "exec".into(),
            "--json".into(),
            "-C".into(),
            directory.to_string_lossy().into_owned(),
            "--skip-git-repo-check".into(),
            "--sandbox".into(),
            sandbox.into(),
            prompt.into(),
        ],
        AgentKind::OpenCode => {
            let mut args = vec![
                "run".into(),
                "--format".into(),
                "json".into(),
                "--dir".into(),
                directory.to_string_lossy().into_owned(),
            ];
            if sandbox == "danger-full-access" {
                args.push("--auto".into());
            }
            args.push(prompt.into());
            args
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
        let args = request.arguments.unwrap_or_default();
        let result = match self
            .dispatch(&principal.0, request.name.as_ref(), &args)
            .await
        {
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

#[cfg(test)]
mod tests {
    use super::agent_arguments;
    use crate::config::AgentKind;
    use std::path::Path;

    #[test]
    fn agent_adapters_build_distinct_cli_arguments() {
        let codex = agent_arguments(
            AgentKind::Codex,
            "read-only",
            Path::new("/tmp/work"),
            "hello",
        );
        assert_eq!(codex[0], "exec");
        assert!(codex.iter().any(|arg| arg == "--sandbox"));
        let opencode = agent_arguments(
            AgentKind::OpenCode,
            "danger-full-access",
            Path::new("/tmp/work"),
            "hello",
        );
        assert_eq!(&opencode[..3], ["run", "--format", "json"]);
        assert!(opencode.iter().any(|arg| arg == "--auto"));
        assert!(!opencode.iter().any(|arg| arg == "--sandbox"));
    }
}
