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
    pub telegram: TelegramConfig,
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
pub struct TelegramConfig {
    /// Enable Telegram bot instead of StdinBot.
    pub enabled: bool,
    /// Bot token from @BotFather.
    pub bot_token: String,
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
            telegram: TelegramConfig::default(),
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

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================
    // Config defaults tests
    // ============================================

    #[test]
    fn config_default_admin_wxid_empty() {
        let config = Config::default();
        assert!(config.admin_wxid.is_empty());
    }

    #[test]
    fn config_default_claude_cli_path() {
        let config = ClaudeConfig::default();
        assert_eq!(config.cli_path, "claude");
    }

    #[test]
    fn config_default_claude_timeout() {
        let config = ClaudeConfig::default();
        assert_eq!(config.timeout, 120);
    }

    #[test]
    fn config_default_session_expire_minutes() {
        let config = SessionConfig::default();
        assert_eq!(config.expire_minutes, 60);
    }

    #[test]
    fn config_default_session_max_history() {
        let config = SessionConfig::default();
        assert_eq!(config.max_history, 50);
    }

    #[test]
    fn config_default_rate_limits() {
        let config = RateLimitConfig::default();
        assert_eq!(config.max_per_minute, 10);
        assert_eq!(config.max_per_day, 200);
    }

    #[test]
    fn config_default_security_no_blocked_patterns() {
        let config = SecurityConfig::default();
        assert!(config.blocked_patterns.is_empty());
        assert!(config.trusted_file_access);
    }

    #[test]
    fn config_default_permissions() {
        let config = PermissionsConfig::default();
        assert!(config.notify_unauthorized);
        assert_eq!(config.default_level, "normal");
        // Unauthorized message should be non-empty Chinese text
        assert!(!config.unauthorized_message.is_empty());
    }

    #[test]
    fn config_default_logging() {
        let config = LoggingConfig::default();
        assert_eq!(config.level, "info");
        assert_eq!(config.file, "logs/bridge.log");
        assert!(config.log_message_content);
    }

    #[test]
    fn config_default_docker_image() {
        let config = crate::config::DockerConfig::default();
        assert_eq!(config.image, "claude-sandbox:latest");
        assert_eq!(config.container_prefix, "claude-friend-");
    }

    #[test]
    fn config_default_docker_limits() {
        let config = crate::config::DockerLimits::default();
        assert_eq!(config.memory, "512m");
        assert_eq!(config.admin_memory, "2g");
        assert_eq!(config.cpus, 1);
        assert_eq!(config.admin_cpus, 2);
        assert_eq!(config.pids, 100);
        assert_eq!(config.tmp_size, "100m");
    }

    #[test]
    fn config_default_docker_network() {
        let config = DockerNetwork::default();
        assert_eq!(config.admin, "bridge");
        assert_eq!(config.trusted, "claude-limited");
        assert_eq!(config.normal, "none");
    }

    // ============================================
    // expanded_data_dir tests
    // ============================================

    #[test]
    fn expanded_data_dir_tilde_prefix() {
        let config = crate::config::DockerConfig {
            data_dir: "~/my-data".into(),
            ..Default::default()
        };
        let expanded = config.expanded_data_dir();
        // Should not start with ~ after expansion
        assert!(!expanded.to_string_lossy().starts_with('~'));
        // Should end with my-data
        assert!(expanded.to_string_lossy().ends_with("my-data"));
    }

    #[test]
    fn expanded_data_dir_absolute_path() {
        let config = crate::config::DockerConfig {
            data_dir: "/absolute/path".into(),
            ..Default::default()
        };
        let expanded = config.expanded_data_dir();
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn expanded_data_dir_relative_path() {
        let config = crate::config::DockerConfig {
            data_dir: "relative/path".into(),
            ..Default::default()
        };
        let expanded = config.expanded_data_dir();
        assert_eq!(expanded, PathBuf::from("relative/path"));
    }

    #[test]
    fn expanded_data_dir_tilde_only() {
        let config = crate::config::DockerConfig {
            data_dir: "~".into(),
            ..Default::default()
        };
        let expanded = config.expanded_data_dir();
        // ~ alone: strip_prefix("~/") fails, so uses data_dir[1..] which is ""
        // home.join("") = home directory itself
        assert!(!expanded.to_string_lossy().contains('~'));
    }

    #[test]
    fn expanded_data_dir_tilde_slash() {
        let config = crate::config::DockerConfig {
            data_dir: "~/".into(),
            ..Default::default()
        };
        let expanded = config.expanded_data_dir();
        assert!(!expanded.to_string_lossy().starts_with('~'));
    }

    // ============================================
    // YAML deserialization tests
    // ============================================

    #[test]
    fn config_deserialize_empty_yaml() {
        // Empty YAML should use all defaults via #[serde(default)]
        let config: Config = serde_yaml::from_str("{}").unwrap();
        assert!(config.admin_wxid.is_empty());
        assert_eq!(config.claude.timeout, 120);
        assert_eq!(config.rate_limit.max_per_minute, 10);
    }

    #[test]
    fn config_deserialize_partial_yaml() {
        let yaml = r#"
admin_wxid: "wx_admin_123"
claude:
  timeout: 300
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.admin_wxid, "wx_admin_123");
        assert_eq!(config.claude.timeout, 300);
        // Other fields should be defaults
        assert_eq!(config.claude.cli_path, "claude");
        assert_eq!(config.rate_limit.max_per_day, 200);
    }

    #[test]
    fn config_deserialize_security_patterns() {
        let yaml = r#"
security:
  blocked_patterns:
    - "rm\\s+-rf"
    - "sudo"
    - "chmod\\s+777"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.security.blocked_patterns.len(), 3);
        assert_eq!(config.security.blocked_patterns[0], "rm\\s+-rf");
    }

    #[test]
    fn config_deserialize_unicode_admin_wxid() {
        let yaml = r#"
admin_wxid: "wxid_中文管理员"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.admin_wxid, "wxid_中文管理员");
    }
}
