use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::database::{Database, Friend, Session};
use crate::docker_manager::{
    ContainerInfo, ContainerStats, DockerManager, ExecClaudeOptions, Permission,
};

/// Maximum response length before truncation (WeChat message friendly).
const MAX_RESPONSE_LEN: usize = 4000;

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 char boundary.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Claude Code executor backed by Docker containers.
///
/// Each user's Claude runs in an isolated container. The executor manages:
/// - Session lifecycle (create, expire, resume)
/// - Concurrency guard (one request per user at a time)
/// - System prompt construction
/// - Response truncation for WeChat
pub struct ClaudeExecutor {
    docker: Arc<DockerManager>,
    db: Arc<Database>,
    /// Set of wxids currently being processed (concurrency guard).
    active_tasks: Mutex<HashSet<String>>,
    /// Session expiry in minutes.
    session_expire_minutes: u64,
    /// Claude execution timeout in seconds.
    timeout: u64,
}

impl ClaudeExecutor {
    pub fn new(
        docker: Arc<DockerManager>,
        db: Arc<Database>,
        session_expire_minutes: u64,
        timeout: u64,
    ) -> Self {
        Self {
            docker,
            db,
            active_tasks: Mutex::new(HashSet::new()),
            session_expire_minutes,
            timeout,
        }
    }

    // ============================================
    // Session management
    // ============================================

    /// Get the active session for a user, creating one if none exists or expired.
    fn get_or_create_session(&self, wxid: &str) -> Result<Session> {
        let session = self.db.session_get_active(wxid)?;

        if let Some(ref s) = session {
            // Check expiry
            if let Some(ref last_active) = s.last_active {
                if is_session_expired(last_active, self.session_expire_minutes) {
                    info!("Session expired, creating new session: {}", wxid);
                    self.db.session_clear_user(wxid)?;
                    return self.create_new_session(wxid);
                }
            }
            return Ok(s.clone());
        }

        self.create_new_session(wxid)
    }

    fn create_new_session(&self, wxid: &str) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        self.db.session_create(&session_id, wxid, None)?;
        info!("Created new session: {} -> {}", wxid, session_id);
        self.db
            .session_get_active(wxid)?
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve newly created session"))
    }

    // ============================================
    // System prompt construction
    // ============================================

    /// Build the system prompt with user identity and permission info.
    fn build_system_prompt(&self, friend: &Friend) -> String {
        let display_name = friend
            .remark_name
            .as_deref()
            .or(friend.nickname.as_deref())
            .unwrap_or(&friend.wxid);

        let perm_desc = match friend.permission.as_str() {
            "admin" => "Admin with full privileges, can execute any code and system operations",
            "trusted" => {
                "Trusted user, can execute code and file operations (within sandbox)"
            }
            "normal" => {
                "Normal user, limited to Q&A only, no code execution or file system access"
            }
            _ => "Unknown permission level",
        };

        let tool_note = if friend.permission == "normal" {
            "- WARNING: This user is limited to Q&A only. Do not execute any code, shell commands, or file operations"
        } else {
            "- This user can request code execution and file operations"
        };

        format!(
            "Current user identity:\n\
             - WeChat ID: {wxid}\n\
             - Nickname: {name}\n\
             - Permission level: {perm} ({perm_desc})\n\
             \n\
             Environment:\n\
             - You are running in this user's dedicated Docker container\n\
             - Working directory: /home/sandbox/workspace (persistent storage)\n\
             - Container is fully isolated from other users\n\
             {tool_note}\n\
             - Keep responses concise, suitable for WeChat reading",
            wxid = friend.wxid,
            name = display_name,
            perm = friend.permission,
            perm_desc = perm_desc,
            tool_note = tool_note,
        )
    }

    // ============================================
    // Core execution
    // ============================================

    /// Execute a user's message through Claude in their Docker container.
    ///
    /// Steps:
    /// 1. Concurrency guard (one request per user)
    /// 2. Ensure container is running
    /// 3. Get/create session
    /// 4. Build system prompt
    /// 5. Execute Claude in container
    /// 6. Extract session ID from stderr
    /// 7. Truncate response if needed
    pub async fn execute(
        &self,
        wxid: &str,
        friend: &Friend,
        message: &str,
    ) -> String {
        // Concurrency guard
        {
            let mut tasks = self.active_tasks.lock().await;
            if tasks.contains(wxid) {
                return "Previous message is still being processed, please wait...".to_string();
            }
            tasks.insert(wxid.to_string());
        }

        let result = self.execute_inner(wxid, friend, message).await;

        // Release concurrency guard
        {
            let mut tasks = self.active_tasks.lock().await;
            tasks.remove(wxid);
        }

        result
    }

    async fn execute_inner(
        &self,
        wxid: &str,
        friend: &Friend,
        message: &str,
    ) -> String {
        let permission = parse_permission(&friend.permission);

        // 1. Ensure container
        if let Err(e) = self.docker.ensure_container(wxid, permission).await {
            error!("Failed to ensure container for {}: {}", wxid, e);
            return "Container setup failed, please try again later".to_string();
        }

        // 2. Get/create session
        let session = match self.get_or_create_session(wxid) {
            Ok(s) => s,
            Err(e) => {
                error!("Session error for {}: {}", wxid, e);
                return "Session error, please try again".to_string();
            }
        };

        // Touch session (update last_active, increment count)
        if let Err(e) = self.db.session_touch(&session.id) {
            warn!("Failed to touch session {}: {}", session.id, e);
        }

        // 3. Build system prompt
        let system_prompt = self.build_system_prompt(friend);

        // 4. Execute Claude in container
        debug!(
            "Executing Claude in container [{}]: {}...",
            wxid,
            truncate_str(message, 80)
        );

        let options = ExecClaudeOptions {
            timeout: Some(self.timeout),
            claude_session: session.claude_session.clone(),
            permission: Some(permission),
        };

        let result = self
            .docker
            .exec_claude(wxid, &system_prompt, message, options)
            .await;

        // 5. Try to extract Claude session ID from stderr
        if !result.stderr.is_empty() {
            self.try_extract_session_id(&session.id, &result.stderr);
        }

        // 6. Truncate if needed
        let mut response = result.output;
        if response.len() > MAX_RESPONSE_LEN {
            // Find a valid char boundary at or before MAX_RESPONSE_LEN
            let mut end = MAX_RESPONSE_LEN;
            while end > 0 && !response.is_char_boundary(end) {
                end -= 1;
            }
            response.truncate(end);
            response.push_str("\n\n... (response truncated)");
        }

        response
    }

    /// Try to extract a Claude session ID from stderr output.
    fn try_extract_session_id(&self, session_id: &str, stderr: &str) {
        let re = Regex::new(r"(?i)session[:\s]+([a-f0-9-]+)").unwrap();
        if let Some(captures) = re.captures(stderr) {
            if let Some(claude_session) = captures.get(1) {
                let cs = claude_session.as_str();
                if let Err(e) = self.db.session_set_claude_session(session_id, cs) {
                    warn!("Failed to save Claude session ID: {}", e);
                } else {
                    debug!("Captured Claude session ID: {}", cs);
                }
            }
        }
    }

    // ============================================
    // Container management proxies
    // ============================================

    /// Clear a user's session, optionally restarting their container.
    pub async fn clear_session(&self, wxid: &str, restart_container: bool) -> Result<()> {
        self.db.session_clear_user(wxid)?;
        if restart_container {
            let _ = self.docker.stop_container(wxid).await;
            self.docker
                .ensure_container(wxid, Permission::Normal)
                .await?;
        }
        info!(
            "Cleared session: {}{}",
            wxid,
            if restart_container {
                " (container restarted)"
            } else {
                ""
            }
        );
        Ok(())
    }

    /// Kill any running Claude process in a user's container.
    pub async fn kill_process(&self, wxid: &str) -> bool {
        match self
            .docker
            .exec_command(wxid, "pkill -f claude || true", true)
            .await
        {
            Ok(_) => {
                let mut tasks = self.active_tasks.lock().await;
                tasks.remove(wxid);
                true
            }
            Err(_) => false,
        }
    }

    /// Get status info for a user's container.
    pub async fn get_container_status(&self, wxid: &str) -> ContainerStatus {
        let name = self.docker.container_name(wxid);
        let running = self.docker.is_running(&name).await;
        let stats = if running {
            self.docker.get_stats(wxid).await.ok().flatten()
        } else {
            None
        };
        let disk = if running {
            self.docker
                .exec_command(wxid, "du -sh /home/sandbox/workspace", false)
                .await
                .ok()
        } else {
            None
        };

        ContainerStatus {
            name,
            running,
            stats,
            disk,
        }
    }

    /// Stop a user's container.
    pub async fn stop_container(&self, wxid: &str) -> Result<bool> {
        self.docker.stop_container(wxid).await
    }

    /// Destroy a user's container (data volumes are preserved).
    pub async fn destroy_container(&self, wxid: &str) -> Result<bool> {
        self.db.session_clear_user(wxid)?;
        {
            let mut tasks = self.active_tasks.lock().await;
            tasks.remove(wxid);
        }
        self.docker.destroy_container(wxid).await
    }

    /// Rebuild a user's container.
    pub async fn rebuild_container(&self, wxid: &str, permission: Permission) -> Result<()> {
        self.db.session_clear_user(wxid)?;
        {
            let mut tasks = self.active_tasks.lock().await;
            tasks.remove(wxid);
        }
        self.docker.rebuild(wxid, permission).await
    }

    /// List all bridge containers.
    pub async fn list_containers(&self) -> Result<Vec<ContainerInfo>> {
        self.docker.list_containers().await
    }
}

/// Container status info.
#[derive(Debug)]
pub struct ContainerStatus {
    pub name: String,
    pub running: bool,
    pub stats: Option<ContainerStats>,
    pub disk: Option<String>,
}

/// Parse a permission string to the Permission enum.
pub fn parse_permission(s: &str) -> Permission {
    match s {
        "admin" => Permission::Admin,
        "trusted" => Permission::Trusted,
        _ => Permission::Normal,
    }
}

/// Check if a session's last_active timestamp is older than expire_minutes.
fn is_session_expired(last_active: &str, expire_minutes: u64) -> bool {
    use chrono::{NaiveDateTime, Utc};

    // Try parsing the SQLite datetime format
    let parsed = NaiveDateTime::parse_from_str(last_active, "%Y-%m-%d %H:%M:%S");
    match parsed {
        Ok(dt) => {
            let now = Utc::now().naive_utc();
            let elapsed = now.signed_duration_since(dt);
            // If elapsed is negative (future timestamp), session is not expired
            let minutes = elapsed.num_minutes();
            if minutes < 0 {
                return false;
            }
            minutes as u64 > expire_minutes
        }
        Err(_) => {
            // If we can't parse, treat as expired
            warn!("Could not parse last_active timestamp: {}", last_active);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================
    // parse_permission tests
    // ============================================

    #[test]
    fn parse_permission_admin() {
        assert_eq!(parse_permission("admin"), Permission::Admin);
    }

    #[test]
    fn parse_permission_trusted() {
        assert_eq!(parse_permission("trusted"), Permission::Trusted);
    }

    #[test]
    fn parse_permission_normal() {
        assert_eq!(parse_permission("normal"), Permission::Normal);
    }

    #[test]
    fn parse_permission_unknown_defaults_to_normal() {
        assert_eq!(parse_permission("blocked"), Permission::Normal);
        assert_eq!(parse_permission(""), Permission::Normal);
        assert_eq!(parse_permission("ADMIN"), Permission::Normal); // case sensitive
        assert_eq!(parse_permission("superuser"), Permission::Normal);
    }

    // ============================================
    // is_session_expired tests
    // ============================================

    #[test]
    fn session_expired_old_timestamp() {
        // A timestamp from 2020 should be expired with any reasonable window
        assert!(is_session_expired("2020-01-01 00:00:00", 60));
    }

    #[test]
    fn session_not_expired_recent() {
        use chrono::Utc;
        // Current timestamp should not be expired with a 60 minute window
        let now = Utc::now().naive_utc();
        let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
        assert!(!is_session_expired(&ts, 60));
    }

    #[test]
    fn session_expired_with_zero_window() {
        use chrono::Utc;
        // Even a current timestamp should be expired with 0 minute window
        // (since elapsed.num_minutes() == 0, and 0 > 0 is false)
        let now = Utc::now().naive_utc();
        let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
        // 0 > 0 is false, so "not expired"
        assert!(!is_session_expired(&ts, 0));
    }

    #[test]
    fn session_expired_invalid_format() {
        // Invalid format should be treated as expired
        assert!(is_session_expired("not-a-date", 60));
        assert!(is_session_expired("", 60));
        assert!(is_session_expired("2024-13-01 00:00:00", 60)); // invalid month
    }

    #[test]
    fn session_expired_iso8601_format_not_supported() {
        // The code only supports "%Y-%m-%d %H:%M:%S", not ISO 8601 with T
        assert!(is_session_expired("2024-01-01T00:00:00", 60));
    }

    #[test]
    fn session_expired_max_window() {
        // Very old timestamp with max u64 window should not expire
        assert!(!is_session_expired("2020-01-01 00:00:00", u64::MAX));
    }

    #[test]
    fn session_expired_future_timestamp() {
        // A future timestamp should not be expired
        assert!(!is_session_expired("2099-01-01 00:00:00", 60));
    }
}
