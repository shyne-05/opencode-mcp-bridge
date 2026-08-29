use std::{collections::HashSet, env, net::IpAddr, path::PathBuf, time::Duration};
use url::Url;

pub const DEFAULT_BACKEND_URL: &str = "http://127.0.0.1:4097";
pub const DEFAULT_PORT: u16 = 3000;
pub const MAX_REQUEST_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    PersonalDesktop,
    ServerSecure,
}

impl Profile {
    fn from_env() -> Result<Self, String> {
        match env::var("MCP_PROFILE")
            .unwrap_or_else(|_| "personal-desktop".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "personal-desktop" | "desktop" | "personal" => Ok(Self::PersonalDesktop),
            "server-secure" | "server" | "secure" => Ok(Self::ServerSecure),
            value => Err(format!(
                "invalid MCP_PROFILE '{value}'; expected personal-desktop or server-secure"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustProxy {
    None,
    Cloudflare,
}

impl TrustProxy {
    fn from_env() -> Result<Self, String> {
        match env::var("MCP_TRUST_PROXY")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "none" | "false" | "off" => Ok(Self::None),
            "cloudflare" => Ok(Self::Cloudflare),
            value => Err(format!(
                "invalid MCP_TRUST_PROXY '{value}'; expected none or cloudflare"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub id: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct ToolConfig {
    pub shell: bool,
    pub browser: bool,
}

#[derive(Debug, Clone)]
pub struct ProcessConfig {
    pub shell_timeout: Duration,
    pub browser_timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub shell_concurrency: usize,
    pub browser_concurrency: usize,
    pub child_env_allowlist: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub public_url: Option<String>,
    pub username: String,
    pub password: Option<String>,
    pub access_token_ttl: u64,
    pub refresh_token_ttl: u64,
    pub code_ttl: u64,
    pub max_failed_logins: usize,
    pub max_login_buckets: usize,
    pub max_authorization_codes: usize,
    pub max_clients: usize,
    pub max_access_tokens: usize,
    pub max_refresh_tokens: usize,
    pub dcr_client_ttl: u64,
    pub client_metadata_timeout: Duration,
    pub client_metadata_max_bytes: usize,
    pub client_metadata_cache_ttl: u64,
    pub login_window: Duration,
}

impl OAuthConfig {
    pub fn enabled(&self) -> bool {
        self.public_url.is_some() && self.password.is_some()
    }

    pub fn public_resource(&self) -> Option<String> {
        self.public_url.as_ref().map(|url| format!("{url}/mcp"))
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub profile: Profile,
    pub host: String,
    pub port: u16,
    pub workdir: PathBuf,
    pub backend_url: String,
    pub backend_response_limit: usize,
    pub max_sessions_per_principal: usize,
    pub browser_script: PathBuf,
    pub node_path: Option<String>,
    pub trust_proxy: TrustProxy,
    pub state_file: Option<PathBuf>,
    pub tokens: Vec<AccessToken>,
    pub allow_unauthenticated: bool,
    pub tools: ToolConfig,
    pub process: ProcessConfig,
    pub oauth: OAuthConfig,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let profile = Profile::from_env()?;
        let workdir =
            std::fs::canonicalize(env::var("BRIDGE_WORKDIR").unwrap_or_else(|_| ".".to_string()))
                .map_err(|error| format!("invalid BRIDGE_WORKDIR: {error}"))?;

        let legacy_host_tools = env_bool("MCP_ENABLE_HOST_TOOLS", false);
        let defaults_enabled = matches!(profile, Profile::PersonalDesktop) && legacy_host_tools;
        let tools = ToolConfig {
            shell: env_bool("MCP_ENABLE_SHELL", defaults_enabled),
            browser: env_bool("MCP_ENABLE_BROWSER", defaults_enabled),
        };

        let oauth = OAuthConfig {
            public_url: validated_public_url()?,
            username: env::var("MCP_OAUTH_USERNAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "admin".to_string()),
            password: env::var("MCP_OAUTH_PASSWORD")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            access_token_ttl: env_u64("MCP_OAUTH_ACCESS_TOKEN_TTL", 3_600, 60, 86_400)?,
            refresh_token_ttl: env_u64("MCP_OAUTH_REFRESH_TOKEN_TTL", 2_592_000, 300, 31_536_000)?,
            code_ttl: env_u64("MCP_OAUTH_CODE_TTL", 300, 60, 900)?,
            max_failed_logins: env_usize("MCP_OAUTH_MAX_FAILED_LOGINS", 6, 1, 50)?,
            max_login_buckets: env_usize("MCP_OAUTH_MAX_LOGIN_BUCKETS", 1_024, 16, 100_000)?,
            max_authorization_codes: env_usize("MCP_OAUTH_MAX_CODES", 256, 16, 100_000)?,
            max_clients: env_usize("MCP_OAUTH_MAX_CLIENTS", 1_024, 16, 100_000)?,
            max_access_tokens: env_usize("MCP_OAUTH_MAX_ACCESS_TOKENS", 1_024, 16, 100_000)?,
            max_refresh_tokens: env_usize("MCP_OAUTH_MAX_REFRESH_TOKENS", 1_024, 16, 100_000)?,
            dcr_client_ttl: env_u64("MCP_OAUTH_DCR_CLIENT_TTL", 2_592_000, 3_600, 31_536_000)?,
            client_metadata_timeout: Duration::from_secs(env_u64(
                "MCP_OAUTH_CLIENT_METADATA_TIMEOUT_SECONDS",
                10,
                2,
                30,
            )?),
            client_metadata_max_bytes: env_usize(
                "MCP_OAUTH_CLIENT_METADATA_MAX_BYTES",
                65_536,
                4_096,
                1_048_576,
            )?,
            client_metadata_cache_ttl: env_u64(
                "MCP_OAUTH_CLIENT_METADATA_CACHE_TTL",
                300,
                30,
                3_600,
            )?,
            login_window: Duration::from_secs(env_u64(
                "MCP_OAUTH_LOGIN_WINDOW_SECONDS",
                60,
                10,
                3_600,
            )?),
        };

        if oauth.public_url.is_some() != oauth.password.is_some() {
            return Err(
                "MCP_PUBLIC_URL and MCP_OAUTH_PASSWORD must either both be set or both be unset"
                    .to_string(),
            );
        }
        if oauth.enabled()
            && oauth
                .password
                .as_ref()
                .is_some_and(|password| password.chars().count() < 12)
        {
            return Err("MCP_OAUTH_PASSWORD must be at least 12 characters".to_string());
        }

        let tokens = configured_tokens();
        let allow_unauthenticated = env_bool("MCP_ALLOW_UNAUTHENTICATED", false);
        if tokens.is_empty() && !oauth.enabled() && !allow_unauthenticated {
            return Err("MCP_TOKEN, MCP_TOKENS, or complete OAuth configuration is required. Set MCP_ALLOW_UNAUTHENTICATED=true only for local development.".to_string());
        }

        let mut child_env_allowlist = default_child_env(profile);
        for name in env::var("MCP_CHILD_ENV_ALLOW")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if is_secret_env_name(name) {
                return Err(format!(
                    "MCP_CHILD_ENV_ALLOW refuses secret-looking variable '{name}'"
                ));
            }
            child_env_allowlist.insert(name.to_string());
        }

        let host = env::var("MCP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        if allow_unauthenticated && !is_loopback_host(&host) {
            return Err("MCP_ALLOW_UNAUTHENTICATED=true is permitted only on a loopback MCP_HOST (127.0.0.1, ::1, or localhost)".to_string());
        }

        let state_file = state_file_path(&workdir);

        Ok(Self {
            profile,
            host,
            port: env_u16("MCP_PORT", DEFAULT_PORT)?,
            workdir,
            backend_url: env::var("BRIDGE_BACKEND_URL")
                .unwrap_or_else(|_| DEFAULT_BACKEND_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            backend_response_limit: env_usize(
                "MCP_BACKEND_RESPONSE_LIMIT_BYTES",
                1_048_576,
                16_384,
                16_777_216,
            )?,
            max_sessions_per_principal: env_usize(
                "MCP_MAX_SESSIONS_PER_PRINCIPAL",
                256,
                1,
                10_000,
            )?,
            browser_script: browser_script_path(),
            node_path: env::var("NODE_PATH")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            trust_proxy: TrustProxy::from_env()?,
            state_file,
            tokens,
            allow_unauthenticated,
            tools,
            process: ProcessConfig {
                shell_timeout: Duration::from_secs(env_u64(
                    "MCP_SHELL_TIMEOUT_SECONDS",
                    30,
                    1,
                    600,
                )?),
                browser_timeout: Duration::from_secs(env_u64(
                    "MCP_BROWSER_TIMEOUT_SECONDS",
                    30,
                    1,
                    300,
                )?),
                stdout_limit: env_usize("MCP_STDOUT_LIMIT_BYTES", 1_048_576, 4_096, 16_777_216)?,
                stderr_limit: env_usize("MCP_STDERR_LIMIT_BYTES", 262_144, 4_096, 4_194_304)?,
                shell_concurrency: env_usize("MCP_SHELL_CONCURRENCY", 2, 1, 16)?,
                browser_concurrency: env_usize("MCP_BROWSER_CONCURRENCY", 1, 1, 8)?,
                child_env_allowlist,
            },
            oauth,
        })
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTH",
        "COOKIE",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
        || upper.starts_with("MCP_")
        || upper.starts_with("CLOUDFLARE_")
}

fn default_child_env(profile: Profile) -> HashSet<String> {
    let mut values = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<HashSet<_>>();

    if matches!(profile, Profile::PersonalDesktop) {
        for name in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_SESSION_TYPE",
            "XDG_CURRENT_DESKTOP",
            "DESKTOP_SESSION",
            "PULSE_SERVER",
            "PIPEWIRE_REMOTE",
        ] {
            values.insert(name.to_string());
        }
    }
    values
}

fn configured_tokens() -> Vec<AccessToken> {
    if let Ok(raw) = env::var("MCP_TOKENS") {
        let tokens = raw
            .split(',')
            .enumerate()
            .filter_map(|(index, entry)| {
                let entry = entry.trim();
                if entry.is_empty() {
                    return None;
                }
                let (id, value) = match entry.split_once('=') {
                    Some((id, value)) if !id.trim().is_empty() && !value.trim().is_empty() => {
                        (id.trim().to_string(), value.trim().to_string())
                    }
                    _ => (format!("user-{}", index + 1), entry.to_string()),
                };
                Some(AccessToken { id, value })
            })
            .collect::<Vec<_>>();
        if !tokens.is_empty() {
            return tokens;
        }
    }

    env::var("MCP_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            vec![AccessToken {
                id: "default".to_string(),
                value: value.trim().to_string(),
            }]
        })
        .unwrap_or_default()
}

fn validated_public_url() -> Result<Option<String>, String> {
    let Some(value) = env::var("MCP_PUBLIC_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    validate_public_url_value(&value, env_bool("MCP_OAUTH_ALLOW_INSECURE_HTTP", false)).map(Some)
}

fn validate_public_url_value(value: &str, allow_insecure: bool) -> Result<String, String> {
    let parsed = Url::parse(value).map_err(|_| {
        "MCP_PUBLIC_URL must be a valid HTTPS origin without a path, query, fragment, or userinfo"
            .to_string()
    })?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "MCP_PUBLIC_URL must include a host".to_string())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("MCP_PUBLIC_URL must not contain username/password userinfo".to_string());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(
            "MCP_PUBLIC_URL must be an origin without a path, query, or fragment".to_string(),
        );
    }
    match parsed.scheme() {
        "https" => {}
        "http" if allow_insecure && matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") => {
        }
        _ => {
            return Err("MCP_PUBLIC_URL must use HTTPS (loopback HTTP is allowed only with MCP_OAUTH_ALLOW_INSECURE_HTTP=true)".to_string());
        }
    }
    Ok(parsed.origin().ascii_serialization())
}

fn browser_script_path() -> PathBuf {
    if let Ok(path) = env::var("MCP_BROWSER_SCRIPT") {
        return PathBuf::from(path);
    }
    if let Ok(executable) = env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let adjacent = parent.join("browser.cjs");
        if adjacent.is_file() {
            return adjacent;
        }
    }
    PathBuf::from("scripts/browser.cjs")
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn state_file_path(workdir: &std::path::Path) -> Option<PathBuf> {
    if let Ok(value) = env::var("MCP_STATE_FILE") {
        let value = value.trim();
        if value.eq_ignore_ascii_case(":memory:") || value.eq_ignore_ascii_case("memory") {
            return None;
        }
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    if let Some(base) = env::var("XDG_STATE_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Some(PathBuf::from(base).join("mcp-bridge/state.json"));
    }
    if let Some(home) = env::var("HOME").ok().filter(|v| !v.trim().is_empty()) {
        return Some(PathBuf::from(home).join(".local/state/mcp-bridge/state.json"));
    }
    Some(workdir.join(".mcp-bridge-state.json"))
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn env_u16(name: &str, default: u16) -> Result<u16, String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| format!("{name} must be a valid u16"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    let value = env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{name} must be an integer"))
        })
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> Result<usize, String> {
    let value = env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("{name} must be an integer"))
        })
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{
        Profile, default_child_env, is_loopback_host, is_secret_env_name, validate_public_url_value,
    };

    #[test]
    fn public_url_requires_a_clean_origin() {
        assert_eq!(
            validate_public_url_value("https://example.com/", false).unwrap(),
            "https://example.com"
        );
        assert_eq!(
            validate_public_url_value("http://127.0.0.1:3000", true).unwrap(),
            "http://127.0.0.1:3000"
        );
        assert!(validate_public_url_value("https://", false).is_err());
        assert!(validate_public_url_value("https://user:pass@example.com", false).is_err());
        assert!(validate_public_url_value("https://example.com/path", false).is_err());
        assert!(validate_public_url_value("http://example.com", true).is_err());
        assert!(validate_public_url_value("http://127.0.0.1:3000", false).is_err());
    }

    #[test]
    fn desktop_environment_is_profile_scoped() {
        let desktop = default_child_env(Profile::PersonalDesktop);
        let server = default_child_env(Profile::ServerSecure);
        assert!(desktop.contains("DISPLAY"));
        assert!(desktop.contains("DBUS_SESSION_BUS_ADDRESS"));
        assert!(desktop.contains("XDG_RUNTIME_DIR"));
        assert!(!server.contains("DISPLAY"));
        assert!(!server.contains("DBUS_SESSION_BUS_ADDRESS"));
        assert!(server.contains("PATH"));
        assert!(server.contains("HOME"));
    }

    #[test]
    fn unauthenticated_listener_requires_loopback() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.2"));
    }

    #[test]
    fn detects_secret_environment_names() {
        for name in [
            "MCP_TOKEN",
            "MCP_OAUTH_PASSWORD",
            "OPENAI_API_KEY",
            "MY_SECRET",
            "CLOUDFLARE_TOKEN",
        ] {
            assert!(is_secret_env_name(name), "{name}");
        }
        for name in ["PATH", "HOME", "DISPLAY", "WAYLAND_DISPLAY", "LANG"] {
            assert!(!is_secret_env_name(name), "{name}");
        }
    }
}
