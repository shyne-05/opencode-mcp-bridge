mod auth;
mod backend;
mod browser;
mod config;
mod durable;
mod oauth;
mod process;
mod state;
mod tools;
mod util;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use backend::Backend;
use config::{Config, MAX_REQUEST_BYTES, Profile};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use state::AppState;
use tokio_util::sync::CancellationToken;
use tools::BridgeServer;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

fn allowed_mcp_hosts(config: &Config) -> Vec<String> {
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if !config.host.is_empty() {
        hosts.push(config.host.clone());
    }
    if let Some(public_url) = config.oauth.public_url.as_deref()
        && let Ok(url) = Url::parse(public_url)
        && let Some(host) = url.host_str()
    {
        hosts.push(host.to_string());
    }
    hosts.sort_unstable();
    hosts.dedup();
    hosts
}

async fn reject_oversized_content_length(request: Request, next: Next) -> Response {
    let oversized = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_REQUEST_BYTES as u64);
    if oversized {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    next.run(request).await
}

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "name": "mcp-bridge",
        "version": env!("CARGO_PKG_VERSION"),
        "build": {
            "commit": env!("MCP_BUILD_COMMIT"),
            "dirty": env!("MCP_BUILD_DIRTY") == "true",
            "browser_helper_protocol": browser::HELPER_PROTOCOL,
        },
        "runtime": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "shell": process::native_shell_name(),
        },
        "mcp": "/mcp",
        "profile": match state.config.profile {
            Profile::PersonalDesktop => "personal-desktop",
            Profile::ServerSecure => "server-secure",
        },
        "authentication": if state.config.oauth.enabled() {
            "oauth2-and-bearer-token"
        } else if state.config.allow_unauthenticated && state.config.tokens.is_empty() {
            "development-unauthenticated"
        } else {
            "bearer-token"
        }
    }))
}

async fn live() -> impl IntoResponse {
    Json(json!({"ok": true}))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let backend = Backend::new(&state).health().await;
    let status = if backend {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(json!({"ok": backend, "backend": backend})))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env().unwrap_or_else(|error| {
        eprintln!("configuration error: {error}");
        std::process::exit(2);
    });
    let address = config.address();
    let state = AppState::new(config).unwrap_or_else(|error| {
        eprintln!("startup error: {error}");
        std::process::exit(2);
    });
    oauth::spawn_cleanup_task(state.clone());
    if state.config.tools.browser {
        let browser_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = browser::warm_browser_worker(&browser_state).await {
                warn!(%error, "failed to prewarm persistent browser worker");
            }
        });
    }

    let cancellation_token = CancellationToken::new();
    let mcp_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_mcp_hosts(&state.config))
        .with_json_response(true)
        .with_cancellation_token(cancellation_token.clone());
    let server_state = state.clone();
    let mcp_service: StreamableHttpService<BridgeServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(BridgeServer::new(server_state.clone())),
            Default::default(),
            mcp_config,
        );

    let protected = Router::new()
        .route_service("/mcp", mcp_service.clone())
        .route_service("/mcp/{token}", mcp_service)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_mcp_auth,
        ));

    let app = Router::new()
        .route("/", get(index))
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::authorization_server_metadata),
        )
        .route(
            "/oauth/authorize",
            get(oauth::authorize_get).post(oauth::authorize_post),
        )
        .route("/oauth/token", post(oauth::token))
        .route("/oauth/register", post(oauth::register))
        .merge(protected)
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn(reject_oversized_content_length))
        .with_state(state.clone());

    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("failed to bind {address}: {error}");
            std::process::exit(2);
        }
    };

    info!(
        address = %address,
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        native_shell = process::native_shell_name(),
        shell = state.config.tools.shell,
        browser = state.config.tools.browser,
        "mcp-bridge started"
    );
    if state.config.allow_unauthenticated {
        warn!("MCP_ALLOW_UNAUTHENTICATED is enabled; use only for local development");
    }

    let shutdown = async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "failed to install Ctrl-C handler");
        }
        cancellation_token.cancel();
    };

    if let Err(error) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    {
        eprintln!("server error: {error}");
        std::process::exit(1);
    }
}
