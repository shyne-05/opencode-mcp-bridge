use crate::{
    state::AppState,
    util::{now_millis, trunc},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct PromptRequest<'a> {
    pub prompt: &'a str,
    pub session_id: Option<&'a str>,
    pub directory: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    pub asynchronous: bool,
}

pub struct Backend<'a> {
    state: &'a AppState,
}

impl<'a> Backend<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.state.config.backend_url, path)
    }

    pub async fn health(&self) -> bool {
        self.state
            .http
            .get(self.url("/global/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    pub async fn read_file(&self, path: &str) -> Result<String, String> {
        let path = resolve_existing_file(&self.state.config.workdir, path)?;
        let response = self
            .state
            .http
            .get(self.url("/file/content"))
            .query(&[("path", path.to_string_lossy().as_ref())])
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;
        let text =
            response_text_checked(response, self.state.config.backend_response_limit).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(content) = value.get("content").and_then(Value::as_str) {
                return Ok(trunc(content, 15_000));
            }
            if let Some(content) = value.get("text").and_then(Value::as_str) {
                return Ok(trunc(content, 15_000));
            }
        }
        Ok(trunc(&text, 15_000))
    }

    pub async fn search(&self, pattern: &str) -> Result<String, String> {
        let directory = self.state.config.workdir.to_string_lossy().into_owned();
        let response = self
            .state
            .http
            .get(self.url("/find"))
            .query(&[("pattern", pattern), ("directory", directory.as_str())])
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;
        let text =
            response_text_checked(response, self.state.config.backend_response_limit).await?;
        filter_search_results(&self.state.config.workdir, &text)
    }

    pub async fn list_sessions(&self, principal: &str) -> Result<String, String> {
        let response = self
            .state
            .http
            .get(self.url("/session"))
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;
        let text =
            response_text_checked(response, self.state.config.backend_response_limit).await?;
        let owned = self.state.owned_sessions(principal).await;
        let Value::Array(items) = serde_json::from_str::<Value>(&text)
            .map_err(|_| "backend returned an invalid session list".to_string())?
        else {
            return Err("backend returned an invalid session list".to_string());
        };
        serde_json::to_string(
            &items
                .into_iter()
                .filter(|item| {
                    item.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| owned.contains(id))
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| format!("failed to encode session list: {error}"))
    }

    pub async fn session_messages(
        &self,
        principal: &str,
        session_id: &str,
        limit: u64,
    ) -> Result<String, String> {
        self.require_session(principal, session_id).await?;
        let response = self
            .state
            .http
            .get(self.url(&format!(
                "/session/{}/message",
                urlencoding::encode(session_id)
            )))
            .query(&[("limit", limit.min(100))])
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;
        Ok(trunc(
            &response_text_checked(response, self.state.config.backend_response_limit).await?,
            15_000,
        ))
    }

    pub async fn session_status(
        &self,
        principal: &str,
        session_id: &str,
    ) -> Result<String, String> {
        self.require_session(principal, session_id).await?;
        let directory = self.state.config.workdir.to_string_lossy().into_owned();
        let response = self
            .state
            .http
            .get(self.url("/session/status"))
            .query(&[("directory", directory)])
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;
        let text =
            response_text_checked(response, self.state.config.backend_response_limit).await?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("backend returned invalid session status JSON: {error}"))?;
        let status = value
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| json!({"type": "idle"}));
        serde_json::to_string(&status)
            .map_err(|error| format!("failed to encode session status: {error}"))
    }

    pub async fn prompt(
        &self,
        principal: &str,
        request: PromptRequest<'_>,
    ) -> Result<String, String> {
        let directory = resolve_directory(&self.state.config.workdir, request.directory)?;
        let mut session_id = request.session_id.unwrap_or_default().to_string();
        if !session_id.is_empty() {
            self.require_session(principal, &session_id).await?;
        }

        let prompt = if session_id.is_empty() {
            format!(
                "{}\n\nUser request: {}",
                include_str!("../system_prompt.md"),
                request.prompt
            )
        } else {
            request.prompt.to_string()
        };

        if session_id.is_empty() {
            let response = self
                .state
                .http
                .post(self.url("/session"))
                .json(&json!({"title": format!("mcp-bridge-{}", now_millis())}))
                .send()
                .await
                .map_err(|error| format!("failed to create backend session: {error}"))?;
            let value =
                response_json_checked(response, self.state.config.backend_response_limit).await?;
            session_id = value
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .get("data")
                        .and_then(|data| data.get("id"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_string();
            if session_id.is_empty() {
                return Err("failed to create backend session".to_string());
            }
            self.state.remember_session(principal, &session_id).await;
        }

        let mut body = json!({"parts": [{"type": "text", "text": prompt}]});
        if let Some(agent) = request.agent {
            body["agent"] = json!(agent);
        }
        if let Some((provider, model_id)) = request.model.and_then(|value| value.split_once('/')) {
            body["model"] = json!({"providerID": provider, "modelID": model_id});
        }
        let directory = directory.to_string_lossy().into_owned();
        let endpoint = if request.asynchronous {
            "prompt_async"
        } else {
            "message"
        };
        let response = self
            .state
            .http
            .post(self.url(&format!(
                "/session/{}/{}",
                urlencoding::encode(&session_id),
                endpoint
            )))
            .query(&[("directory", directory)])
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;

        if request.asynchronous {
            if response.status().is_success() {
                return Ok(format!("Async request sent for session {session_id}"));
            }
            return Err(format!("backend returned status {}", response.status()));
        }

        let text =
            response_text_checked(response, self.state.config.backend_response_limit).await?;
        if let Ok(value) = serde_json::from_str::<Value>(&text)
            && let Some(parts) = value.get("parts").and_then(Value::as_array)
        {
            let answer = parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !answer.is_empty() {
                return Ok(format!("Session:{session_id}\n{answer}"));
            }
        }
        Ok(format!("Session:{}\n{}", session_id, trunc(&text, 8_000)))
    }

    async fn require_session(&self, principal: &str, session_id: &str) -> Result<(), String> {
        if session_id.is_empty() || !self.state.owns_session(principal, session_id).await {
            Err("session does not belong to this authenticated user".to_string())
        } else {
            Ok(())
        }
    }
}

fn filter_search_results(base: &Path, text: &str) -> Result<String, String> {
    let Value::Array(items) = serde_json::from_str::<Value>(text)
        .map_err(|_| "backend returned an invalid search result list".to_string())?
    else {
        return Err("backend returned an invalid search result list".to_string());
    };
    let filtered = items
        .into_iter()
        .filter(|item| {
            let Some(path) = item
                .get("path")
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
            else {
                return false;
            };
            resolve_existing_path(base, path).is_ok()
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&filtered)
        .map(|value| trunc(&value, 15_000))
        .map_err(|error| format!("failed to encode search results: {error}"))
}

pub fn resolve_existing_file(base: &Path, requested: &str) -> Result<PathBuf, String> {
    let resolved = resolve_existing_path(base, requested)?;
    if !resolved.is_file() {
        return Err(format!(
            "path is not a regular file: {}",
            resolved.display()
        ));
    }
    Ok(resolved)
}

pub fn resolve_directory(base: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let candidate = requested
        .map(PathBuf::from)
        .unwrap_or_else(|| base.to_path_buf());
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    };
    let resolved = std::fs::canonicalize(&candidate)
        .map_err(|error| format!("invalid directory '{}': {error}", candidate.display()))?;
    if !resolved.is_dir() {
        return Err(format!(
            "directory is not a directory: {}",
            resolved.display()
        ));
    }
    ensure_inside(base, &resolved)?;
    Ok(resolved)
}

pub fn resolve_existing_path(base: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        base.join(requested)
    };
    let resolved = std::fs::canonicalize(&candidate)
        .map_err(|error| format!("invalid path '{}': {error}", candidate.display()))?;
    ensure_inside(base, &resolved)?;
    Ok(resolved)
}

fn ensure_inside(base: &Path, resolved: &Path) -> Result<(), String> {
    if resolved.starts_with(base) {
        Ok(())
    } else {
        Err(format!(
            "path must be inside BRIDGE_WORKDIR ({})",
            base.display()
        ))
    }
}

async fn response_text_checked(
    response: reqwest::Response,
    limit: usize,
) -> Result<String, String> {
    let (status, bytes, truncated_body) = read_response_limited(response, limit).await?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if !status.is_success() {
        return Err(format!(
            "backend returned status {status}: {}",
            trunc(&text, 2_000)
        ));
    }
    if truncated_body {
        text.push_str("\n[backend response truncated at configured byte limit]");
    }
    Ok(text)
}

async fn response_json_checked(response: reqwest::Response, limit: usize) -> Result<Value, String> {
    let (status, bytes, truncated_body) = read_response_limited(response, limit).await?;
    let text = String::from_utf8_lossy(&bytes);
    if !status.is_success() {
        return Err(format!(
            "backend returned status {status}: {}",
            trunc(&text, 2_000)
        ));
    }
    if truncated_body {
        return Err(format!(
            "backend JSON response exceeded MCP_BACKEND_RESPONSE_LIMIT_BYTES ({limit})"
        ));
    }
    serde_json::from_str(&text).map_err(|error| format!("backend returned invalid JSON: {error}"))
}

async fn read_response_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(reqwest::StatusCode, Vec<u8>, bool), String> {
    let status = response.status();
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read backend response: {error}"))?
    {
        let remaining = limit.saturating_sub(bytes.len());
        let keep = remaining.min(chunk.len());
        bytes.extend_from_slice(&chunk[..keep]);
        if keep < chunk.len() {
            truncated = true;
            break;
        }
    }
    Ok((status, bytes, truncated))
}

#[cfg(test)]
mod tests {
    use super::{
        filter_search_results, read_response_limited, resolve_directory, resolve_existing_file,
        resolve_existing_path,
    };
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn backend_response_reader_caps_streamed_bytes() {
        use axum::{Router, routing::get};

        let app = Router::new().route("/large", get(|| async { "x".repeat(32 * 1024) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let address = listener.local_addr().expect("test address should resolve");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run");
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/large"))
            .send()
            .await
            .expect("test request should complete");
        let (status, body, truncated) = read_response_limited(response, 1024)
            .await
            .expect("bounded response should read");
        assert!(status.is_success());
        assert_eq!(body.len(), 1024);
        assert!(truncated);
        server.abort();
    }

    #[test]
    fn rejects_paths_outside_workdir() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, "secret").unwrap();
        assert!(resolve_existing_path(root.path(), file.to_str().unwrap()).is_err());
    }

    #[test]
    fn accepts_paths_inside_workdir() {
        let root = tempdir().unwrap();
        let file = root.path().join("ok.txt");
        fs::write(&file, "ok").unwrap();
        assert_eq!(resolve_existing_path(root.path(), "ok.txt").unwrap(), file);
        assert_eq!(resolve_directory(root.path(), None).unwrap(), root.path());
    }

    #[test]
    fn read_file_rejects_directories() {
        let root = tempdir().unwrap();
        assert!(resolve_existing_file(root.path(), ".").is_err());
    }

    #[test]
    fn bridge_search_respects_workdir() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(root.path().join("inside.txt"), "inside").unwrap();
        fs::write(outside.path().join("outside.txt"), "outside").unwrap();
        let input = serde_json::json!([
            {"path":{"text":"inside.txt"},"lines":{"text":"inside"}},
            {"path":{"text": outside.path().join("outside.txt").to_string_lossy()},"lines":{"text":"outside"}}
        ]).to_string();
        let filtered: serde_json::Value =
            serde_json::from_str(&filter_search_results(root.path(), &input).unwrap()).unwrap();
        let items = filtered.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["path"]["text"], "inside.txt");
    }

    #[cfg(unix)]
    #[test]
    fn bridge_search_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let file = outside.path().join("outside.txt");
        fs::write(&file, "OUTSIDE").unwrap();
        symlink(&file, root.path().join("link.txt")).unwrap();
        let input = serde_json::json!([{"path":{"text":"link.txt"},"lines":{"text":"OUTSIDE"}}])
            .to_string();
        let filtered: serde_json::Value =
            serde_json::from_str(&filter_search_results(root.path(), &input).unwrap()).unwrap();
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, "secret").unwrap();
        symlink(&file, root.path().join("link.txt")).unwrap();
        assert!(resolve_existing_path(root.path(), "link.txt").is_err());
    }
}
