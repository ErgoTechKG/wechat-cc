use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Top-level configuration, deserialized from config.yaml.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub admin_wxid: String,
    pub claude: ClaudeConfig,
    pub docker: DockerConfig,
    pub permissions: PermissionsConfig,
    pub session: SessionConfig,
    pub rate_limit: RateLimitConfig,
    pub security: SecurityConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ClaudeConfig {
    pub cli_path: String,
    pub timeout: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct DockerConfig {
    pub image: String,
    pub container_prefix: String,
    pub data_dir: String,
    pub limits: DockerLimits,
    pub network: DockerNetwork,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct DockerLimits {
    pub memory: String,
    pub admin_memory: String,
    pub cpus: u32,
    pub admin_cpus: u32,
    pub pids: u32,
    pub tmp_size: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct DockerNetwork {
    pub admin: String,
    pub trusted: String,
    pub normal: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct PermissionsConfig {
    pub notify_unauthorized: bool,
    pub unauthorized_message: String,
    pub default_level: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SessionConfig {
    pub expire_minutes: u64,
    pub max_history: usize,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct RateLimitConfig {
    pub max_per_minute: u32,
    pub max_per_day: u32,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SecurityConfig {
    pub blocked_patterns: Vec<String>,
    pub trusted_file_access: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub file: String,
    pub log_message_content: bool,
}

// --- Default implementations matching the JS version ---

impl Default for Config {
    fn default() -> Self {
        Self {
            admin_wxid: String::new(),
            claude: ClaudeConfig::default(),
            docker: DockerConfig::default(),
            permissions: PermissionsConfig::default(),
            session: SessionConfig::default(),
            rate_limit: RateLimitConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            cli_path: "claude".into(),
            timeout: 120,
        }
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "claude-sandbox:latest".into(),
            container_prefix: "claude-friend-".into(),
            data_dir: "~/claude-bridge-data".into(),
            limits: DockerLimits::default(),
            network: DockerNetwork::default(),
        }
    }
}

impl Default for DockerLimits {
    fn default() -> Self {
        Self {
            memory: "512m".into(),
            admin_memory: "2g".into(),
            cpus: 1,
            admin_cpus: 2,
            pids: 100,
            tmp_size: "100m".into(),
        }
    }
}

impl Default for DockerNetwork {
    fn default() -> Self {
        Self {
            admin: "bridge".into(),
            trusted: "claude-limited".into(),
            normal: "none".into(),
        }
    }
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            notify_unauthorized: true,
            unauthorized_message: "抱歉，你还没有被授权使用此服务。".into(),
            default_level: "normal".into(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            expire_minutes: 60,
            max_history: 50,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_per_minute: 10,
            max_per_day: 200,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            blocked_patterns: Vec::new(),
            trusted_file_access: true,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            file: "logs/bridge.log".into(),
            log_message_content: true,
        }
    }
}

impl DockerConfig {
    /// Returns data_dir with ~ expanded to the user's home directory.
    pub fn expanded_data_dir(&self) -> PathBuf {
        if self.data_dir.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                return home.join(self.data_dir.strip_prefix("~/").unwrap_or(&self.data_dir[1..]));
            }
        }
        PathBuf::from(&self.data_dir)
    }
}

/// Load configuration from config.yaml at the project root.
/// Panics if config.yaml is missing or malformed. Call once at startup.
pub fn load_config() -> Result<Config> {
    let config_path = PathBuf::from("config.yaml");
    if !config_path.exists() {
        anyhow::bail!(
            "config.yaml not found. Please copy config.example.yaml to config.yaml and edit it."
        );
    }
    let contents =
        std::fs::read_to_string(&config_path).context("Failed to read config.yaml")?;
    let config: Config = serde_yaml::from_str(&contents).context("Failed to parse config.yaml")?;
    Ok(config)
}

/// Initialize the global config. Returns an error if config.yaml is missing or invalid.
/// Must be called once before `get_config()`.
pub fn init_config() -> Result<()> {
    let config = load_config()?;
    CONFIG
        .set(config)
        .map_err(|_| anyhow::anyhow!("Config already initialized"))?;
    Ok(())
}

/// Get a reference to the global config. Panics if `init_config()` was not called.
pub fn get_config() -> &'static Config {
    CONFIG.get().expect("Config not initialized. Call init_config() first.")
}
