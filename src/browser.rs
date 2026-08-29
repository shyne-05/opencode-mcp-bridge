use crate::{
    process::{run_program, run_program_with_env},
    state::AppState,
    util::{optional_string_arg, required_string_arg, trunc},
};
use url::Url;

const CDP: &str = "http://127.0.0.1:9222";
pub const HELPER_PROTOCOL: &str = "mcp-browser-helper/2";

pub async fn run_browser_action(
    state: &AppState,
    action: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, String> {
    let _permit = state
        .browser_slots
        .acquire()
        .await
        .map_err(|_| "browser concurrency limiter closed".to_string())?;
    let text = match action {
        "tabs" => list_tabs(state).await?,
        "new" => {
            let url = args
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("about:blank");
            safe_browser_url(url)?;
            state
                .http
                .put(format!("{CDP}/json/new?{}", urlencoding::encode(url)))
                .send()
                .await
                .map_err(|error| format!("browser request failed: {error}"))?
                .text()
                .await
                .map_err(|error| format!("browser response failed: {error}"))?
        }
        "navigate" => {
            let url = args
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "url is required".to_string())?;
            safe_browser_url(url)?;
            run_script(
                state,
                "navigate",
                optional_string_arg(args, "targetId"),
                &[url],
            )
            .await?
        }
        "close" => {
            let target_id = required_string_arg(args, "targetId")?;
            ensure_page_target_exists(state, target_id).await?;
            let response = state
                .http
                .get(format!(
                    "{CDP}/json/close/{}",
                    urlencoding::encode(target_id)
                ))
                .send()
                .await
                .map_err(|error| format!("browser request failed: {error}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| format!("browser response failed: {error}"))?;
            if !status.is_success() || body.contains("No such target id") {
                return Err(format!(
                    "failed to close browser target {target_id}: {}",
                    body.trim()
                ));
            }
            body
        }
        "snapshot" => {
            run_script(
                state,
                "snapshot",
                optional_string_arg(args, "targetId"),
                &[],
            )
            .await?
        }
        "click" => {
            run_script(
                state,
                "click",
                optional_string_arg(args, "targetId"),
                &[required_string_arg(args, "selector")?],
            )
            .await?
        }
        "fill" => {
            let selector = required_string_arg(args, "selector")?;
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            run_script(
                state,
                "fill",
                optional_string_arg(args, "targetId"),
                &[selector, value],
            )
            .await?
        }
        "evaluate" => {
            run_script(
                state,
                "evaluate",
                optional_string_arg(args, "targetId"),
                &[required_string_arg(args, "expression")?],
            )
            .await?
        }
        _ => return Err(format!("unknown browser action: {action}")),
    };
    Ok(trunc(&text, 15_000))
}

async fn fetch_targets(state: &AppState) -> Result<serde_json::Value, String> {
    let response = state
        .http
        .get(format!("{CDP}/json/list"))
        .send()
        .await
        .map_err(|error| format!("browser request failed: {error}"))?;
    let status = response.status();
    let value = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("browser response was not valid JSON: {error}"))?;
    if !status.is_success() {
        return Err(format!("browser returned status {status}"));
    }
    Ok(value)
}

async fn list_tabs(state: &AppState) -> Result<String, String> {
    page_targets(fetch_targets(state).await?)
}

async fn ensure_page_target_exists(state: &AppState, target_id: &str) -> Result<(), String> {
    if page_target_exists(&fetch_targets(state).await?, target_id)? {
        Ok(())
    } else {
        Err(format!("browser page target not found: {target_id}"))
    }
}

fn page_target_exists(value: &serde_json::Value, target_id: &str) -> Result<bool, String> {
    let serde_json::Value::Array(items) = value else {
        return Err("browser target list was not an array".to_string());
    };
    Ok(items.iter().any(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("page")
            && item.get("id").and_then(serde_json::Value::as_str) == Some(target_id)
    }))
}

fn page_targets(value: serde_json::Value) -> Result<String, String> {
    let serde_json::Value::Array(items) = value else {
        return Err("browser target list was not an array".to_string());
    };
    let tabs = items
        .into_iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("page"))
        .map(|item| {
            serde_json::json!({
                "id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "title": item.get("title").cloned().unwrap_or(serde_json::Value::Null),
                "url": item.get("url").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({
        "count": tabs.len(),
        "tabs": tabs,
    }))
    .map_err(|error| format!("failed to encode browser tabs: {error}"))
}

async fn run_script(
    state: &AppState,
    action: &str,
    target_id: Option<&str>,
    args: &[&str],
) -> Result<String, String> {
    if !state.config.browser_script.is_file() {
        return Err(format!(
            "browser script not found: {}",
            state.config.browser_script.display()
        ));
    }
    ensure_helper_compatible(state).await?;
    let mut arguments = vec![
        state.config.browser_script.to_string_lossy().into_owned(),
        action.to_string(),
        target_id.unwrap_or_default().to_string(),
    ];
    arguments.extend(args.iter().map(|value| (*value).to_string()));
    let node_path = discover_node_path(state).await;
    let extra_env = node_path
        .as_deref()
        .map(|path| [("NODE_PATH", path)])
        .unwrap_or([("NODE_PATH", "")]);
    let output = run_program_with_env(
        "node",
        &arguments,
        None,
        state.config.process.browser_timeout,
        &state.config.process,
        &extra_env,
    )
    .await;
    if output.code == Some(0) && !output.timed_out {
        Ok(output.render())
    } else {
        Err(output.render())
    }
}

async fn ensure_helper_compatible(state: &AppState) -> Result<(), String> {
    state.browser_helper_check.get_or_init(|| async {
        let node_path = discover_node_path(state).await;
        let extra_env = node_path.as_deref().map(|path| [("NODE_PATH", path)]).unwrap_or([("NODE_PATH", "")]);
        let output = run_program_with_env(
            "node",
            &[state.config.browser_script.to_string_lossy().into_owned(), "version".to_string()],
            None,
            state.config.process.browser_timeout,
            &state.config.process,
            &extra_env,
        ).await;
        if !output.is_success() { return Err(format!("browser helper version check failed: {}", output.render())); }
        let actual = output.stdout.trim();
        if actual != HELPER_PROTOCOL {
            return Err(format!("browser helper protocol mismatch: bridge expects {HELPER_PROTOCOL}, helper reports {actual}"));
        }
        Ok(())
    }).await.clone()
}

async fn discover_node_path(state: &AppState) -> Option<String> {
    state
        .node_path
        .get_or_init(|| async {
            if let Some(path) = state.config.node_path.clone() {
                return Some(path);
            }
            let output = run_program(
                if cfg!(windows) { "npm.cmd" } else { "npm" },
                &["root".to_string(), "-g".to_string()],
                None,
                state.config.process.browser_timeout,
                &state.config.process,
            )
            .await;
            (output.code == Some(0) && !output.timed_out)
                .then(|| output.stdout.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .await
        .clone()
}

pub fn safe_browser_url(url: &str) -> Result<&str, String> {
    if url == "about:blank" {
        return Ok(url);
    }
    let parsed = Url::parse(url)
        .map_err(|_| "browser URL must be a valid http:// or https:// URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("browser URLs must use http://, https://, or about:blank".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("browser URLs with username/password userinfo are not allowed".to_string());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{page_target_exists, page_targets, safe_browser_url};
    use serde_json::json;

    #[test]
    fn tabs_only_include_page_targets() {
        let rendered = page_targets(json!([
            {"type":"page","id":"p1","title":"One","url":"https://one.example"},
            {"type":"iframe","id":"f1","title":"Frame","url":"https://frame.example"},
            {"type":"worker","id":"w1","title":"Worker","url":"https://worker.example"},
            {"type":"page","id":"p2","title":"Two","url":"https://two.example"}
        ]))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["count"], 2);
        assert_eq!(parsed["tabs"][0]["id"], "p1");
        assert_eq!(parsed["tabs"][1]["id"], "p2");
    }

    #[test]
    fn missing_browser_page_target_is_detected() {
        let targets = json!([
            {"type":"page","id":"real"},
            {"type":"iframe","id":"frame"}
        ]);
        assert!(page_target_exists(&targets, "real").unwrap());
        assert!(!page_target_exists(&targets, "missing").unwrap());
        assert!(!page_target_exists(&targets, "frame").unwrap());
    }

    #[test]
    fn only_allows_safe_browser_urls() {
        assert!(safe_browser_url("about:blank").is_ok());
        assert!(safe_browser_url("https://example.com").is_ok());
        assert!(safe_browser_url("http://127.0.0.1:4097/health").is_ok());
        assert!(safe_browser_url("https://").is_err());
        assert!(safe_browser_url("https://user:pass@example.com").is_err());
        assert!(safe_browser_url("file:///etc/passwd").is_err());
        assert!(safe_browser_url("javascript:alert(1)").is_err());
    }
}
