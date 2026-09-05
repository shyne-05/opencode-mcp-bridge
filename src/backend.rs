use crate::{
    state::AppState,
    util::{now_millis, trunc},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

const SEARCH_OUTPUT_LIMIT: usize = 15_000;

pub struct PromptRequest<'a> {
    pub prompt: &'a str,
    pub session_id: Option<&'a str>,
    pub directory: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
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
            .timeout(Duration::from_secs(3))
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
        let workdir = self.state.config.workdir.clone();
        tokio::task::spawn_blocking(move || filter_search_results(&workdir, &text))
            .await
            .map_err(|error| format!("backend search task failed: {error}"))?
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
            self.state.remember_session(principal, &session_id).await?;
        }

        let mut body = json!({"parts": [{"type": "text", "text": prompt}]});
        if let Some(agent) = request.agent {
            body["agent"] = json!(agent);
        }
        if let Some((provider, model_id)) = request.model.and_then(|value| value.split_once('/')) {
            body["model"] = json!({"providerID": provider, "modelID": model_id});
        }
        let directory = directory.to_string_lossy().into_owned();
        let response = self
            .state
            .http
            .post(self.url(&format!(
                "/session/{}/message",
                urlencoding::encode(&session_id)
            )))
            .query(&[("directory", directory)])
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("backend request failed: {error}"))?;

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
    let canonical_base = std::fs::canonicalize(base)
        .map_err(|error| format!("invalid BRIDGE_WORKDIR '{}': {error}", base.display()))?;
    let mut allowed_paths = HashMap::new();
    let mut output = String::from("[");
    let mut output_chars = 2; // Reserve both array brackets.
    for item in &items {
        let Some(path) = item
            .get("path")
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        // Search often returns many matches in the same file. Resolve each path
        // once for this response, while still checking symlinks against the root.
        let allowed = allowed_paths.entry(path).or_insert_with(|| {
            std::fs::canonicalize(base.join(path))
                .is_ok_and(|resolved| resolved.starts_with(&canonical_base))
        });
        if !*allowed {
            continue;
        }
        let encoded = serde_json::to_string(item)
            .map_err(|error| format!("failed to encode search results: {error}"))?;
        let separator = usize::from(output.len() > 1);
        let remaining = SEARCH_OUTPUT_LIMIT - output_chars;
        let encoded_chars = encoded.chars().take(remaining + 1).count();
        if encoded_chars + separator > remaining {
            break;
        }
        if separator != 0 {
            output.push(',');
        }
        output.push_str(&encoded);
        output_chars += encoded_chars + separator;
    }
    // Limit whole entries so even a capped response remains valid JSON.
    output.push(']');
    Ok(output)
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
    let canonical_base = std::fs::canonicalize(base)
        .map_err(|error| format!("invalid BRIDGE_WORKDIR '{}': {error}", base.display()))?;
    if resolved.starts_with(&canonical_base) {
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
    let mut text = String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
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

pub(crate) async fn read_response_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(reqwest::StatusCode, Vec<u8>, bool), String> {
    let status = response.status();
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(8 * 1024)
        .min(limit)
        .min(64 * 1024);
    let mut bytes = Vec::with_capacity(capacity);
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read HTTP response: {error}"))?
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
        SEARCH_OUTPUT_LIMIT, filter_search_results, read_response_limited, resolve_directory,
        resolve_existing_file, resolve_existing_path, response_json_checked, response_text_checked,
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

    fn test_response(body: impl Into<Vec<u8>>, status: u16) -> reqwest::Response {
        axum::http::Response::builder()
            .status(status)
            .body(body.into())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn backend_response_reader_handles_exact_and_zero_limits() {
        for (body, limit, expected, truncated) in [
            ("hello", 5, "hello", false),
            ("hello", 4, "hell", true),
            ("", 0, "", false),
            ("hello", 0, "", true),
        ] {
            let (_, actual, was_truncated) = read_response_limited(test_response(body, 200), limit)
                .await
                .unwrap();
            assert_eq!(actual, expected.as_bytes());
            assert_eq!(was_truncated, truncated);
        }
    }

    #[tokio::test]
    async fn backend_text_preserves_lossy_utf8_and_status_errors() {
        assert_eq!(
            response_text_checked(test_response("héllo", 200), 100)
                .await
                .unwrap(),
            "héllo"
        );
        assert_eq!(
            response_text_checked(test_response(vec![b'a', 0xff], 200), 100)
                .await
                .unwrap(),
            "a�"
        );
        assert_eq!(
            response_text_checked(test_response("é🦀", 200), 3)
                .await
                .unwrap(),
            "é�\n[backend response truncated at configured byte limit]"
        );
        let error = response_text_checked(test_response("unavailable", 503), 100)
            .await
            .unwrap_err();
        assert!(error.contains("503"));
        assert!(error.ends_with("unavailable"));
    }

    #[tokio::test]
    async fn backend_json_rejects_truncated_payloads() {
        let body = r#"{"id":"session"}"#;
        assert_eq!(
            response_json_checked(test_response(body, 200), body.len())
                .await
                .unwrap()["id"],
            "session"
        );
        let error = response_json_checked(test_response(body, 200), body.len() - 1)
            .await
            .unwrap_err();
        assert!(error.contains("exceeded MCP_BACKEND_RESPONSE_LIMIT_BYTES"));
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
        assert_eq!(
            resolve_existing_path(root.path(), "ok.txt").unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        assert_eq!(
            resolve_directory(root.path(), None).unwrap(),
            fs::canonicalize(root.path()).unwrap()
        );
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

    #[test]
    fn bridge_search_caps_whole_entries_and_keeps_valid_json() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("inside.txt"), "inside").unwrap();
        let input = serde_json::Value::Array(
            (0..300)
                .map(|index| {
                    serde_json::json!({
                        "path": {"text": "inside.txt"},
                        "lines": {"text": "🦀".repeat(100)},
                        "line_number": index,
                    })
                })
                .collect(),
        );
        let output = filter_search_results(root.path(), &input.to_string()).unwrap();
        assert!(output.chars().count() <= SEARCH_OUTPUT_LIMIT);
        let filtered: serde_json::Value = serde_json::from_str(&output).unwrap();
        let filtered = filtered.as_array().unwrap();
        assert!(!filtered.is_empty());
        assert!(filtered.len() < 300);
        for (index, item) in filtered.iter().enumerate() {
            assert_eq!(item, &input[index]);
        }
    }

    #[test]
    fn bridge_search_handles_oversized_entries_and_invalid_items() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("inside.txt"), "inside").unwrap();
        let input = serde_json::json!([
            {},
            {"path": {"text": "missing.txt"}},
            {"path": {"text": "inside.txt"}, "lines": {"text": "x".repeat(SEARCH_OUTPUT_LIMIT)}}
        ]);
        assert_eq!(
            filter_search_results(root.path(), &input.to_string()).unwrap(),
            "[]"
        );
        assert!(filter_search_results(root.path(), "{}").is_err());
        assert!(filter_search_results(root.path(), "invalid").is_err());
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
