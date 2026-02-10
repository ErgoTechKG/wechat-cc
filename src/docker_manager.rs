use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, ListContainersOptions,
    RemoveContainerOptions, StartContainerOptions, Stats, StatsOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::BuildImageOptions;
use bollard::models::{
    ContainerStateStatusEnum, HostConfig, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::network::{CreateNetworkOptions, InspectNetworkOptions};
use bollard::Docker;
use futures_util::StreamExt;
use tokio::fs;
use tracing::{debug, error, info, warn};

/// Docker configuration for container limits, network, and naming.
#[derive(Debug, Clone)]
pub struct DockerConfig {
    pub image: String,
    pub container_prefix: String,
    pub data_dir: PathBuf,
    pub limits: DockerLimits,
    pub network: DockerNetworkConfig,
}

#[derive(Debug, Clone)]
pub struct DockerLimits {
    pub memory: i64,       // bytes (512MB = 536_870_912)
    pub admin_memory: i64, // bytes (2GB = 2_147_483_648)
    pub cpus: i64,         // nano-cpus (1 core = 1_000_000_000)
    pub admin_cpus: i64,   // nano-cpus
    pub pids: i64,
    pub tmp_size: String,  // e.g. "100m"
}

#[derive(Debug, Clone)]
pub struct DockerNetworkConfig {
    pub admin: String,
    pub trusted: String,
    pub normal: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        Self {
            image: "claude-sandbox:latest".to_string(),
            container_prefix: "claude-friend-".to_string(),
            data_dir: home.join("claude-bridge-data"),
            limits: DockerLimits::default(),
            network: DockerNetworkConfig::default(),
        }
    }
}

impl Default for DockerLimits {
    fn default() -> Self {
        Self {
            memory: 512 * 1024 * 1024,        // 512m
            admin_memory: 2 * 1024 * 1024 * 1024, // 2g
            cpus: 1_000_000_000,               // 1 core in nano-cpus
            admin_cpus: 2_000_000_000,         // 2 cores
            pids: 100,
            tmp_size: "100m".to_string(),
        }
    }
}

impl Default for DockerNetworkConfig {
    fn default() -> Self {
        Self {
            admin: "bridge".to_string(),
            trusted: "claude-limited".to_string(),
            normal: "none".to_string(),
        }
    }
}

/// Permission level for a user's container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Normal,
    Trusted,
    Admin,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::Normal => "normal",
            Permission::Trusted => "trusted",
            Permission::Admin => "admin",
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result from executing Claude in a container.
#[derive(Debug)]
pub struct ExecClaudeResult {
    pub ok: bool,
    pub output: String,
    pub stderr: String,
}

/// Options for executing Claude.
#[derive(Debug, Default)]
pub struct ExecClaudeOptions {
    pub timeout: Option<u64>,
    pub claude_session: Option<String>,
    pub permission: Option<Permission>,
}

/// Container info returned by list_containers.
#[derive(Debug)]
pub struct ContainerInfo {
    pub name: String,
    pub status: String,
    pub wxid: Option<String>,
    pub permission: Option<String>,
}

/// Container stats snapshot.
#[derive(Debug)]
pub struct ContainerStats {
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub pids: u64,
}

/// Docker container manager.
///
/// Manages per-user Docker containers for the WeChat-Claude bridge:
/// - Filesystem isolation: each user has their own /home/sandbox/workspace
/// - Process isolation: containers don't affect host or other users
/// - Resource limits: CPU, memory, PID caps
/// - Network isolation: configurable per permission level
/// - Persistence: workspace volumes survive restarts
pub struct DockerManager {
    docker: Docker,
    container_prefix: String,
    image_name: String,
    data_dir: PathBuf,
    config: DockerConfig,
}

impl DockerManager {
    /// Create a new DockerManager with the given config.
    pub async fn new(config: DockerConfig) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Failed to connect to Docker daemon")?;

        // Ensure data root directory exists
        fs::create_dir_all(&config.data_dir)
            .await
            .with_context(|| format!("Failed to create data dir: {:?}", config.data_dir))?;

        Ok(Self {
            docker,
            container_prefix: config.container_prefix.clone(),
            image_name: config.image.clone(),
            data_dir: config.data_dir.clone(),
            config,
        })
    }

    // ============================================
    // Container naming
    // ============================================

    /// Convert wxid to a Docker-safe container name.
    pub fn container_name(&self, wxid: &str) -> String {
        let safe: String = wxid
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}{}", self.container_prefix, safe)
    }

    /// Get (and create) the per-user data directory on the host.
    pub async fn user_data_dir(&self, wxid: &str) -> Result<PathBuf> {
        let dir = self.data_dir.join(wxid);
        fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("Failed to create user data dir: {:?}", dir))?;
        Ok(dir)
    }

    // ============================================
    // Container lifecycle
    // ============================================

    /// Ensure a user's container exists and is running.
    /// Creates it if missing, starts it if stopped.
    pub async fn ensure_container(&self, wxid: &str, permission: Permission) -> Result<String> {
        let name = self.container_name(wxid);

        if !self.container_exists(&name).await {
            self.create_container(wxid, permission).await?;
            info!("Created container: {}", name);
        }

        if !self.is_running(&name).await {
            self.start_container(&name).await?;
            info!("Started container: {}", name);
        }

        Ok(name)
    }

    /// Create a user's container with appropriate resource limits and security settings.
    pub async fn create_container(&self, wxid: &str, permission: Permission) -> Result<()> {
        let name = self.container_name(wxid);
        let data_dir = self.user_data_dir(wxid).await?;

        // Determine resource limits based on permission
        let (memory, nano_cpus) = match permission {
            Permission::Admin => (self.config.limits.admin_memory, self.config.limits.admin_cpus),
            _ => (self.config.limits.memory, self.config.limits.cpus),
        };

        let network = self.get_network(permission);

        let workspace_bind = format!(
            "{}:/home/sandbox/workspace",
            data_dir.join("workspace").display()
        );
        let claude_config_bind = format!(
            "{}:/home/sandbox/.claude",
            data_dir.join("claude-config").display()
        );
        let tmpfs_opt = format!("size={}", self.config.limits.tmp_size);

        let host_config = HostConfig {
            // Resource limits
            memory: Some(memory),
            nano_cpus: Some(nano_cpus),
            pids_limit: Some(self.config.limits.pids),

            // Tmpfs for /tmp
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                tmpfs_opt,
            )])),

            // Security: read-only rootfs, no-new-privileges, drop ALL caps
            readonly_rootfs: Some(true),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),

            // Network
            network_mode: Some(network),

            // Volume mounts
            binds: Some(vec![workspace_bind, claude_config_bind]),

            // Restart policy
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),

            ..Default::default()
        };

        // Labels for batch management
        let mut labels = HashMap::new();
        labels.insert("app", "wechat-claude-bridge");
        labels.insert("wxid", wxid);
        let perm_str = permission.as_str();
        labels.insert("permission", perm_str);

        let env_wxid = format!("WXID={}", wxid);

        // Only pass ANTHROPIC_API_KEY if set. When using Claude Code Max
        // (subscription), auth is handled via OAuth and stored in the
        // mounted ~/.claude volume — no API key needed.
        let mut env_vars = vec![env_wxid.as_str()];
        let env_api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(|k| format!("ANTHROPIC_API_KEY={}", k));
        if let Some(ref key_env) = env_api_key {
            env_vars.push(key_env.as_str());
        }

        let container_config = Config {
            image: Some(self.image_name.as_str()),
            cmd: Some(vec!["tail", "-f", "/dev/null"]),
            env: Some(env_vars),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        self.docker
            .create_container(
                Some(CreateContainerOptions {
                    name: name.as_str(),
                    platform: None,
                }),
                container_config,
            )
            .await
            .with_context(|| format!("Failed to create container: {}", name))?;

        // Start the container so fix_permissions can exec into it
        self.start_container(&name).await?;

        // Fix volume directory permissions
        self.fix_permissions(wxid).await;

        Ok(())
    }

    /// Fix volume directory permissions (host-created dirs may be owned by root).
    async fn fix_permissions(&self, wxid: &str) {
        let data_dir = match self.user_data_dir(wxid).await {
            Ok(d) => d,
            Err(_) => return,
        };

        // Ensure subdirectories exist
        let _ = fs::create_dir_all(data_dir.join("workspace")).await;
        let _ = fs::create_dir_all(data_dir.join("claude-config")).await;

        // Use root exec inside the container to chown
        let name = self.container_name(wxid);
        if let Err(e) = self
            .exec_in_container(
                &name,
                vec![
                    "chown",
                    "-R",
                    "sandbox:sandbox",
                    "/home/sandbox/workspace",
                    "/home/sandbox/.claude",
                ],
                true, // as root
            )
            .await
        {
            debug!("fix_permissions failed (container may not be ready): {}", e);
        }
    }

    /// Get the Docker network name for a permission level.
    fn get_network(&self, permission: Permission) -> String {
        match permission {
            Permission::Admin => self.config.network.admin.clone(),
            Permission::Trusted => self.config.network.trusted.clone(),
            Permission::Normal => self.config.network.normal.clone(),
        }
    }

    // ============================================
    // Execute commands in container
    // ============================================

    /// Execute Claude Code in a user's container. This is the core method.
    pub async fn exec_claude(
        &self,
        wxid: &str,
        system_prompt: &str,
        message: &str,
        options: ExecClaudeOptions,
    ) -> ExecClaudeResult {
        let name = self.container_name(wxid);
        let timeout_secs = options.timeout.unwrap_or(120);

        // Build claude command
        let mut cmd = vec![
            "claude".to_string(),
            "--print".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
            "--system-prompt".to_string(),
            system_prompt.to_string(),
        ];

        // Session resume
        if let Some(ref session) = options.claude_session {
            cmd.push("--resume".to_string());
            cmd.push(session.clone());
        }

        // Permission-based tool restrictions
        if let Some(Permission::Normal) = options.permission {
            cmd.push("--allowedTools".to_string());
            cmd.push(String::new());
        }

        // User message
        cmd.push(message.to_string());

        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();

        // Only pass ANTHROPIC_API_KEY if set. Claude Code Max users
        // authenticate via OAuth stored in ~/.claude (mounted volume).
        let env_api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(|k| format!("ANTHROPIC_API_KEY={}", k));
        let env_refs: Vec<&str> = env_api_key.iter().map(|s| s.as_str()).collect();

        let exec_opts = CreateExecOptions {
            cmd: Some(cmd_refs),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            user: Some("sandbox"),
            working_dir: Some("/home/sandbox/workspace"),
            env: if env_refs.is_empty() { None } else { Some(env_refs) },
            ..Default::default()
        };

        let exec = match self.docker.create_exec(&name, exec_opts).await {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to create exec in container {}: {}", name, e);
                return ExecClaudeResult {
                    ok: false,
                    output: "Container execution failed".to_string(),
                    stderr: e.to_string(),
                };
            }
        };

        // Start exec with timeout
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.collect_exec_output(&exec.id),
        )
        .await;

        match result {
            Ok(Ok((stdout, stderr))) => {
                let trimmed = stdout.trim().to_string();
                if trimmed.is_empty() {
                    ExecClaudeResult {
                        ok: true,
                        output: "(Claude returned no content)".to_string(),
                        stderr,
                    }
                } else {
                    ExecClaudeResult {
                        ok: true,
                        output: trimmed,
                        stderr,
                    }
                }
            }
            Ok(Err(e)) => {
                error!("Container exec failed [{}]: {}", name, e);
                ExecClaudeResult {
                    ok: false,
                    output: "Processing error, please try again later".to_string(),
                    stderr: e.to_string(),
                }
            }
            Err(_) => {
                // Timeout
                warn!("Claude exec timed out in container {} after {}s", name, timeout_secs);
                ExecClaudeResult {
                    ok: false,
                    output: "Request timed out".to_string(),
                    stderr: String::new(),
                }
            }
        }
    }

    /// Execute an arbitrary command in a user's container.
    pub async fn exec_command(
        &self,
        wxid: &str,
        command: &str,
        as_root: bool,
    ) -> Result<String> {
        let name = self.container_name(wxid);
        self.exec_in_container(&name, vec!["sh", "-c", command], as_root)
            .await
    }

    /// Low-level: exec a command array in a named container.
    async fn exec_in_container(
        &self,
        container_name: &str,
        cmd: Vec<&str>,
        as_root: bool,
    ) -> Result<String> {
        let user = if as_root { "root" } else { "sandbox" };

        let exec_opts = CreateExecOptions {
            cmd: Some(cmd),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            user: Some(user),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(container_name, exec_opts)
            .await
            .with_context(|| format!("Failed to create exec in {}", container_name))?;

        let (stdout, stderr) = self.collect_exec_output(&exec.id).await?;

        if !stderr.is_empty() {
            debug!("exec stderr in {}: {}", container_name, stderr);
        }

        Ok(stdout.trim().to_string())
    }

    /// Collect stdout/stderr from a docker exec.
    async fn collect_exec_output(&self, exec_id: &str) -> Result<(String, String)> {
        let start_result = self
            .docker
            .start_exec(exec_id, None)
            .await
            .context("Failed to start exec")?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        match start_result {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(anyhow::anyhow!("Error reading exec output: {}", e));
                        }
                    }
                }
            }
            StartExecResults::Detached => {
                // Nothing to collect
            }
        }

        Ok((stdout, stderr))
    }

    // ============================================
    // Container status queries
    // ============================================

    /// Check if a container exists.
    pub async fn container_exists(&self, name: &str) -> bool {
        self.docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
            .is_ok()
    }

    /// Check if a container is running.
    pub async fn is_running(&self, name: &str) -> bool {
        match self
            .docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => info
                .state
                .and_then(|s| s.status)
                .map(|s| s == ContainerStateStatusEnum::RUNNING)
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Start a container by name.
    pub async fn start_container(&self, name: &str) -> Result<()> {
        self.docker
            .start_container(name, None::<StartContainerOptions<String>>)
            .await
            .with_context(|| format!("Failed to start container: {}", name))
    }

    /// Stop a user's container gracefully (10s timeout).
    pub async fn stop_container(&self, wxid: &str) -> Result<bool> {
        let name = self.container_name(wxid);
        match self
            .docker
            .stop_container(
                &name,
                Some(StopContainerOptions { t: 10 }),
            )
            .await
        {
            Ok(_) => {
                info!("Stopped container: {}", name);
                Ok(true)
            }
            Err(e) => {
                warn!("Failed to stop container {}: {}", name, e);
                Ok(false)
            }
        }
    }

    /// Force-remove a user's container.
    pub async fn destroy_container(&self, wxid: &str) -> Result<bool> {
        let name = self.container_name(wxid);
        match self
            .docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(_) => {
                info!("Destroyed container: {}", name);
                Ok(true)
            }
            Err(e) => {
                warn!("Failed to destroy container {}: {}", name, e);
                Ok(false)
            }
        }
    }

    /// Get resource usage stats for a container.
    pub async fn get_stats(&self, wxid: &str) -> Result<Option<ContainerStats>> {
        let name = self.container_name(wxid);

        let mut stream = self.docker.stats(
            &name,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );

        if let Some(Ok(stats)) = stream.next().await {
            let cpu_percent = calculate_cpu_percent(&stats);
            let memory_usage = stats.memory_stats.usage.unwrap_or(0);
            let memory_limit = stats.memory_stats.limit.unwrap_or(0);
            let pids = stats
                .pids_stats
                .current
                .unwrap_or(0);

            Ok(Some(ContainerStats {
                cpu_percent,
                memory_usage,
                memory_limit,
                pids,
            }))
        } else {
            Ok(None)
        }
    }

    /// List all containers managed by this bridge (label=app=wechat-claude-bridge).
    pub async fn list_containers(&self) -> Result<Vec<ContainerInfo>> {
        let mut filters = HashMap::new();
        filters.insert("label", vec!["app=wechat-claude-bridge"]);

        let options = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .context("Failed to list containers")?;

        let mut result = Vec::new();
        for c in containers {
            let name = c
                .names
                .as_ref()
                .and_then(|n| n.first())
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default();

            let status = c.status.unwrap_or_default();
            let labels = c.labels.unwrap_or_default();
            let wxid = labels.get("wxid").cloned();
            let permission = labels.get("permission").cloned();

            result.push(ContainerInfo {
                name,
                status,
                wxid,
                permission,
            });
        }

        Ok(result)
    }

    // ============================================
    // Batch management
    // ============================================

    /// Stop all bridge containers.
    pub async fn stop_all(&self) -> Result<()> {
        let containers = self.list_containers().await?;
        let count = containers.len();
        for c in &containers {
            if let Some(ref wxid) = c.wxid {
                let _ = self.stop_container(wxid).await;
            }
        }
        info!("Stopped {} containers", count);
        Ok(())
    }

    /// Remove all stopped bridge containers.
    pub async fn cleanup(&self) -> Result<()> {
        let containers = self.list_containers().await?;
        for c in &containers {
            // Only remove non-running containers
            if !c.status.to_lowercase().contains("up") {
                if let Some(ref wxid) = c.wxid {
                    let _ = self.destroy_container(wxid).await;
                }
            }
        }
        info!("Cleanup of stopped bridge containers complete");
        Ok(())
    }

    /// Rebuild a user's container (destroy and recreate).
    pub async fn rebuild(&self, wxid: &str, permission: Permission) -> Result<()> {
        let _ = self.destroy_container(wxid).await;
        self.ensure_container(wxid, permission).await?;
        info!("Rebuilt container: {}", self.container_name(wxid));
        Ok(())
    }

    // ============================================
    // Network initialization
    // ============================================

    /// Create the claude-limited network if it doesn't exist.
    pub async fn init_networks(&self) -> Result<()> {
        let network_name = "claude-limited";

        match self
            .docker
            .inspect_network(network_name, None::<InspectNetworkOptions<String>>)
            .await
        {
            Ok(_) => {
                debug!("Network {} already exists", network_name);
            }
            Err(_) => {
                let options = CreateNetworkOptions {
                    name: network_name,
                    driver: "bridge",
                    ..Default::default()
                };
                match self.docker.create_network(options).await {
                    Ok(_) => {
                        info!("Created network: {}", network_name);
                    }
                    Err(e) => {
                        warn!("Failed to create network {}: {}", network_name, e);
                    }
                }
            }
        }

        Ok(())
    }

    // ============================================
    // Health & image management
    // ============================================

    /// Check if Docker is available and responding.
    pub async fn health_check(&self) -> Result<bool> {
        match self.docker.version().await {
            Ok(version) => {
                let ver = version.version.unwrap_or_else(|| "unknown".to_string());
                info!("Docker version: {}", ver);
                Ok(true)
            }
            Err(e) => {
                error!("Docker is not available: {}", e);
                Ok(false)
            }
        }
    }

    /// Check if the sandbox image exists locally.
    pub async fn image_exists(&self) -> Result<bool> {
        match self.docker.inspect_image(&self.image_name).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Build the sandbox image from the project's docker directory.
    pub async fn build_image(&self, docker_dir: &Path) -> Result<()> {
        info!("Building sandbox image: {}", self.image_name);

        let dockerfile_path = docker_dir.join("Dockerfile.sandbox");
        if !dockerfile_path.exists() {
            return Err(anyhow::anyhow!(
                "Dockerfile not found at {:?}",
                dockerfile_path
            ));
        }

        // Read the Dockerfile and create a tar archive for the build context
        let tar_bytes = create_build_context(docker_dir).await?;

        let build_options = BuildImageOptions {
            t: self.image_name.as_str(),
            dockerfile: "Dockerfile.sandbox",
            rm: true,
            ..Default::default()
        };

        let mut stream = self.docker.build_image(build_options, None, Some(tar_bytes.into()));

        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if let Some(stream_str) = output.stream {
                        debug!("build: {}", stream_str.trim());
                    }
                    if let Some(err) = output.error {
                        return Err(anyhow::anyhow!("Image build error: {}", err));
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Image build failed: {}", e));
                }
            }
        }

        info!("Image build complete: {}", self.image_name);
        Ok(())
    }
}

/// Calculate CPU usage percentage from Docker stats.
fn calculate_cpu_percent(stats: &Stats) -> f64 {
    let cpu_delta = stats.cpu_stats.cpu_usage.total_usage as f64
        - stats.precpu_stats.cpu_usage.total_usage as f64;
    let system_delta = stats.cpu_stats.system_cpu_usage.unwrap_or(0) as f64
        - stats.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let num_cpus = stats
        .cpu_stats
        .online_cpus
        .unwrap_or(1) as f64;

    if system_delta > 0.0 && cpu_delta >= 0.0 {
        (cpu_delta / system_delta) * num_cpus * 100.0
    } else {
        0.0
    }
}

/// Create a tar archive of the docker build context directory.
async fn create_build_context(dir: &Path) -> Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());

    // Walk the directory and add files
    fn add_dir_to_tar(
        builder: &mut tar::Builder<Vec<u8>>,
        dir: &Path,
        prefix: &Path,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = prefix.join(entry.file_name());

            if path.is_dir() {
                add_dir_to_tar(builder, &path, &name)?;
            } else {
                let mut file = std::fs::File::open(&path)?;
                builder.append_file(name, &mut file)?;
            }
        }
        Ok(())
    }

    add_dir_to_tar(&mut archive, dir, Path::new(""))?;
    let bytes = archive.into_inner()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_name_sanitization() {
        let config = DockerConfig::default();
        // We can't create a full DockerManager without Docker, so test the logic directly
        let prefix = &config.container_prefix;
        let wxid = "wxid_abc123";
        let safe: String = wxid
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("{}{}", prefix, safe);
        assert_eq!(name, "claude-friend-wxid_abc123");
    }

    #[test]
    fn test_container_name_special_chars() {
        let prefix = "claude-friend-";
        let wxid = "user@foo/bar";
        let safe: String = wxid
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("{}{}", prefix, safe);
        assert_eq!(name, "claude-friend-user_foo_bar");
    }

    #[test]
    fn test_permission_display() {
        assert_eq!(Permission::Normal.as_str(), "normal");
        assert_eq!(Permission::Trusted.as_str(), "trusted");
        assert_eq!(Permission::Admin.as_str(), "admin");
    }

    #[test]
    fn test_default_config() {
        let config = DockerConfig::default();
        assert_eq!(config.image, "claude-sandbox:latest");
        assert_eq!(config.container_prefix, "claude-friend-");
        assert_eq!(config.limits.memory, 512 * 1024 * 1024);
        assert_eq!(config.limits.admin_memory, 2 * 1024 * 1024 * 1024);
        assert_eq!(config.limits.pids, 100);
        assert_eq!(config.network.normal, "none");
        assert_eq!(config.network.trusted, "claude-limited");
        assert_eq!(config.network.admin, "bridge");
    }
}
