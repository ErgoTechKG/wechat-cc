use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use tracing::{info, warn};

use crate::claude_executor::{parse_permission, ClaudeExecutor};
use crate::config::get_config;
use crate::database::{AuditEntry, Database, Friend};
use crate::wechat_bot::Contact;

// ============================================
// Helpers
// ============================================

/// Permission level numeric value for comparison.
fn perm_level(perm: &str) -> u8 {
    match perm {
        "admin" => 3,
        "trusted" => 2,
        "normal" => 1,
        _ => 0,
    }
}

/// Display name for a Contact: remark_name > nickname > wxid.
fn display_name(contact: &Contact) -> &str {
    if !contact.remark_name.is_empty() {
        &contact.remark_name
    } else if !contact.nickname.is_empty() {
        &contact.nickname
    } else {
        &contact.wxid
    }
}

// ============================================
// Command metadata
// ============================================

struct Command {
    /// Minimum permission required.
    permission: &'static str,
    /// Human-readable description.
    description: &'static str,
}

// ============================================
// MessageRouter
// ============================================

pub struct MessageRouter {
    db: Arc<Database>,
    executor: Arc<ClaudeExecutor>,
    admin_wxid: String,
    /// Command name -> metadata.  Dispatch is via match in handle_command_dispatch.
    commands: HashMap<&'static str, Command>,
}

impl MessageRouter {
    pub fn new(db: Arc<Database>, executor: Arc<ClaudeExecutor>, admin_wxid: String) -> Self {
        let mut commands = HashMap::new();

        // User commands
        commands.insert("/help", Command { permission: "normal", description: "查看帮助" });
        commands.insert("/status", Command { permission: "normal", description: "查看状态（含容器信息）" });
        commands.insert("/clear", Command { permission: "normal", description: "清除会话历史" });

        // Admin commands
        commands.insert("/allow", Command { permission: "admin", description: "授权好友: /allow 昵称 [trusted|normal]" });
        commands.insert("/block", Command { permission: "admin", description: "拉黑好友: /block 昵称" });
        commands.insert("/list", Command { permission: "admin", description: "列出所有授权好友" });
        commands.insert("/logs", Command { permission: "admin", description: "查看日志: /logs [昵称]" });
        commands.insert("/kill", Command { permission: "admin", description: "终止好友进程: /kill 昵称" });
        commands.insert("/containers", Command { permission: "admin", description: "查看所有容器状态" });
        commands.insert("/restart", Command { permission: "admin", description: "重启容器: /restart 昵称" });
        commands.insert("/destroy", Command { permission: "admin", description: "销毁容器（保留数据）: /destroy 昵称" });
        commands.insert("/rebuild", Command { permission: "admin", description: "重建容器: /rebuild 昵称" });
        commands.insert("/stopall", Command { permission: "admin", description: "停止所有容器" });

        Self { db, executor, admin_wxid, commands }
    }

    // ============================================
    // Core routing
    // ============================================

    /// Handle an incoming message and return an optional reply.
    pub async fn handle_message(&self, contact: &Contact, message: &str) -> Option<String> {
        let config = get_config();
        let dn = display_name(contact);

        // 1. Log + audit incoming message
        info!(
            "收到消息 [{}({})]: {}",
            dn,
            contact.wxid,
            &message[..message.len().min(100)]
        );
        let audit_content = if config.logging.log_message_content {
            message
        } else {
            "[已隐藏]"
        };
        let _ = self.db.audit_log(&contact.wxid, Some(dn), "in", Some(audit_content), None);

        // 2. Ensure friend registered
        self.ensure_friend_registered(contact);

        // 3. Permission check
        let permission = self.get_effective_permission(&contact.wxid);

        if permission == "blocked" {
            warn!("拒绝黑名单用户: {}({})", dn, contact.wxid);
            return None;
        }

        // If permission resolves to something unrecognized (empty or unknown), treat as unauthorized
        if perm_level(&permission) == 0 && permission != "blocked" {
            return if config.permissions.notify_unauthorized {
                Some(config.permissions.unauthorized_message.clone())
            } else {
                None
            };
        }

        // 4. Rate limit check
        if let Ok(result) = self.db.rate_limit_check_and_increment(
            &contact.wxid,
            config.rate_limit.max_per_minute as i64,
            config.rate_limit.max_per_day as i64,
        ) {
            if !result.allowed {
                return Some(format!("⚠️ {}", result.reason.unwrap_or_default()));
            }
        }

        // 5. Command handling
        if message.starts_with('/') {
            if let Some(response) = self.handle_command(&contact.wxid, &permission, message).await {
                let _ = self.db.audit_log(
                    &contact.wxid,
                    Some(dn),
                    "out",
                    Some(&response[..response.len().min(200)]),
                    None,
                );
                return Some(response);
            }
        }

        // 6. Security check
        if let Some(reason) = self.security_check(message, &permission) {
            return Some(format!("⚠️ {}", reason));
        }

        // 7. Forward to Claude executor
        let friend = match self.db.friend_get(&contact.wxid) {
            Ok(Some(f)) => f,
            _ => return Some("❌ 处理消息时出错了，请稍后重试".to_string()),
        };

        let response = self.executor.execute(&contact.wxid, &friend, message).await;

        let _ = self.db.audit_log(
            &contact.wxid,
            Some(dn),
            "out",
            Some(&response[..response.len().min(500)]),
            None,
        );
        info!("回复 [{}]: {}...", dn, &response[..response.len().min(100)]);

        Some(response)
    }

    // ============================================
    // Permission helpers
    // ============================================

    fn get_effective_permission(&self, wxid: &str) -> String {
        if wxid == self.admin_wxid {
            return "admin".to_string();
        }
        let config = get_config();
        match self.db.friend_get_permission(wxid) {
            Ok(Some(perm)) => perm,
            _ => config.permissions.default_level.clone(),
        }
    }

    fn ensure_friend_registered(&self, contact: &Contact) {
        let config = get_config();
        match self.db.friend_get(&contact.wxid) {
            Ok(Some(existing)) => {
                let nick_changed = existing.nickname.as_deref() != Some(&contact.nickname);
                let remark_changed = existing.remark_name.as_deref()
                    != if contact.remark_name.is_empty() { None } else { Some(contact.remark_name.as_str()) };
                if nick_changed || remark_changed {
                    let _ = self.db.friend_upsert(
                        &contact.wxid,
                        Some(&contact.nickname),
                        if contact.remark_name.is_empty() { None } else { Some(&contact.remark_name) },
                        None,
                        None,
                        None,
                    );
                }
            }
            Ok(None) => {
                let perm = if contact.wxid == self.admin_wxid {
                    "admin"
                } else {
                    &config.permissions.default_level
                };
                let _ = self.db.friend_upsert(
                    &contact.wxid,
                    Some(&contact.nickname),
                    if contact.remark_name.is_empty() { None } else { Some(&contact.remark_name) },
                    Some(perm),
                    None,
                    None,
                );
                info!("新好友注册: {}({})", display_name(contact), contact.wxid);
            }
            Err(_) => {}
        }
    }

    // ============================================
    // Command dispatch
    // ============================================

    async fn handle_command(&self, wxid: &str, permission: &str, message: &str) -> Option<String> {
        let parts: Vec<&str> = message.trim().split_whitespace().collect();
        let cmd = parts[0].to_lowercase();
        let args = if parts.len() > 1 {
            parts[1..].join(" ")
        } else {
            String::new()
        };

        // Look up command metadata
        let command = self.commands.get(cmd.as_str())?;

        // Permission check
        if perm_level(permission) < perm_level(command.permission) {
            return Some("⚠️ 权限不足".to_string());
        }

        // Dispatch to handler
        let result = match cmd.as_str() {
            "/help" => self.cmd_help(permission),
            "/status" => self.cmd_status(wxid).await,
            "/clear" => self.cmd_clear(wxid).await,
            "/allow" => self.cmd_allow(&args),
            "/block" => self.cmd_block(&args).await,
            "/list" => self.cmd_list(),
            "/logs" => self.cmd_logs(&args),
            "/kill" => self.cmd_kill(&args).await,
            "/containers" => self.cmd_containers().await,
            "/restart" => self.cmd_restart(&args).await,
            "/destroy" => self.cmd_destroy(&args).await,
            "/rebuild" => self.cmd_rebuild(&args).await,
            "/stopall" => self.cmd_stopall().await,
            _ => return None,
        };

        Some(result)
    }

    // ============================================
    // Command implementations - Basic
    // ============================================

    fn cmd_help(&self, permission: &str) -> String {
        let mut lines = vec!["📖 可用命令:\n".to_string()];

        // Collect and sort for stable output
        let mut entries: Vec<_> = self.commands.iter().collect();
        entries.sort_by_key(|(name, _)| **name);

        for (name, cmd) in &entries {
            if perm_level(permission) >= perm_level(cmd.permission) {
                lines.push(format!("{} - {}", name, cmd.description));
            }
        }

        lines.push("\n直接发送文字消息即可与 Claude 对话".to_string());
        lines.join("\n")
    }

    async fn cmd_status(&self, wxid: &str) -> String {
        let friend = self.db.friend_get(wxid).ok().flatten();
        let session = self.db.session_get_active(wxid).ok().flatten();
        let container = self.executor.get_container_status(wxid).await;

        let friend_name = friend
            .as_ref()
            .and_then(|f| f.remark_name.as_deref().or(f.nickname.as_deref()))
            .unwrap_or("未知");
        let friend_perm = friend.as_ref().map(|f| f.permission.as_str()).unwrap_or("无");

        let session_info = match session {
            Some(ref s) => format!("活跃 ({} 条消息)", s.message_count),
            None => "无".to_string(),
        };

        let mut lines = vec![
            "📊 当前状态:\n".to_string(),
            format!("👤 {}", friend_name),
            format!("🔑 权限: {}", friend_perm),
            format!("💬 会话: {}", session_info),
            String::new(),
            format!("🐳 容器: {}", container.name),
            format!(
                "   状态: {}",
                if container.running { "✅ 运行中" } else { "⏹️ 已停止" }
            ),
        ];

        if let Some(ref stats) = container.stats {
            lines.push(format!("   CPU: {:.1}%", stats.cpu_percent));
            lines.push(format!(
                "   内存: {} / {}",
                format_bytes(stats.memory_usage),
                format_bytes(stats.memory_limit)
            ));
            lines.push(format!("   进程: {}", stats.pids));
        }
        if let Some(ref disk) = container.disk {
            lines.push(format!("   磁盘: {}", disk));
        }

        lines.join("\n")
    }

    async fn cmd_clear(&self, wxid: &str) -> String {
        let _ = self.executor.clear_session(wxid, false).await;
        "✅ 会话已清除，下次对话将开始新的上下文".to_string()
    }

    // ============================================
    // Command implementations - Friend management
    // ============================================

    fn cmd_allow(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /allow 昵称 [trusted|normal]".to_string();
        }

        let parts: Vec<&str> = args.split_whitespace().collect();
        let search_name = parts[0];
        let level = parts.get(1).copied().unwrap_or("trusted");

        if !["trusted", "normal", "admin"].contains(&level) {
            return "❌ 无效权限等级，可选: trusted, normal, admin".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(search_name) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };

        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"，该好友需要先发一条消息", search_name);
        }
        if matches.len() > 1 {
            let names: Vec<String> = matches
                .iter()
                .map(|f| format!("{}({})", f.nickname.as_deref().unwrap_or("?"), f.wxid))
                .collect();
            return format!("找到多个匹配:\n{}\n请精确指定", names.join("\n"));
        }

        let friend = &matches[0];
        let _ = self.db.friend_set_permission(&friend.wxid, level);
        let nick = friend.nickname.as_deref().unwrap_or("?");
        info!("权限变更: {} -> {}", nick, level);
        format!("✅ {} → {}", nick, level)
    }

    async fn cmd_block(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /block 昵称".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }
        if matches.len() > 1 {
            return "找到多个匹配，请精确指定".to_string();
        }

        let friend = &matches[0];
        let _ = self.db.friend_set_permission(&friend.wxid, "blocked");
        let _ = self.executor.destroy_container(&friend.wxid).await;
        let nick = friend.nickname.as_deref().unwrap_or("?");
        info!("已拉黑并销毁容器: {}", nick);
        format!("🚫 已拉黑 {}，容器已销毁", nick)
    }

    fn cmd_list(&self) -> String {
        let friends = match self.db.friend_list_all() {
            Ok(f) => f,
            Err(_) => return "❌ 查询出错".to_string(),
        };

        if friends.is_empty() {
            return "暂无授权好友".to_string();
        }

        let mut lines = vec!["👥 好友列表:\n".to_string()];

        // Group by permission
        let mut grouped: HashMap<&str, Vec<&Friend>> = HashMap::new();
        for f in &friends {
            grouped.entry(f.permission.as_str()).or_default().push(f);
        }

        let order = ["admin", "trusted", "normal", "blocked"];
        let icons: HashMap<&str, &str> =
            [("admin", "👑"), ("trusted", "⭐"), ("normal", "👤"), ("blocked", "🚫")].into();

        for perm in &order {
            if let Some(group) = grouped.get(perm) {
                if !group.is_empty() {
                    let icon = icons.get(perm).unwrap_or(&"");
                    lines.push(format!("{} {}:", icon, perm.to_uppercase()));
                    for f in group {
                        let name = f.remark_name.as_deref().or(f.nickname.as_deref()).unwrap_or(&f.wxid);
                        lines.push(format!("  {}", name));
                    }
                    lines.push(String::new());
                }
            }
        }

        lines.join("\n")
    }

    fn cmd_logs(&self, args: &str) -> String {
        if args.is_empty() {
            let logs = self.db.audit_get_recent(20).unwrap_or_default();
            return format_logs(&logs);
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }

        let logs = self.db.audit_get_by_user(&matches[0].wxid, 20).unwrap_or_default();
        format_logs(&logs)
    }

    async fn cmd_kill(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /kill 昵称".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }

        let killed = self.executor.kill_process(&matches[0].wxid).await;
        if killed {
            format!("✅ 已终止 {} 的进程", matches[0].nickname.as_deref().unwrap_or("?"))
        } else {
            "没有运行中的进程".to_string()
        }
    }

    // ============================================
    // Command implementations - Container management
    // ============================================

    async fn cmd_containers(&self) -> String {
        let containers = match self.executor.list_containers().await {
            Ok(c) => c,
            Err(_) => return "❌ 查询容器失败".to_string(),
        };

        if containers.is_empty() {
            return "🐳 暂无运行中的容器".to_string();
        }

        let mut lines = vec!["🐳 容器列表:\n".to_string()];
        for c in &containers {
            let friend = c.wxid.as_ref().and_then(|w| self.db.friend_get(w).ok().flatten());
            let name = friend
                .as_ref()
                .and_then(|f| f.remark_name.as_deref().or(f.nickname.as_deref()))
                .or(c.wxid.as_deref())
                .unwrap_or("未知");
            let perm = c.permission.as_deref().unwrap_or("?");
            let status_icon = if c.status.contains("Up") { "✅" } else { "⏹️" };
            lines.push(format!("{} {} [{}]", status_icon, name, perm));
            lines.push(format!("   {}: {}", c.name, c.status));
        }

        lines.join("\n")
    }

    async fn cmd_restart(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /restart 昵称".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }

        let friend = &matches[0];
        let _ = self.executor.stop_container(&friend.wxid).await;
        let _ = self.executor.clear_session(&friend.wxid, false).await;
        format!(
            "🔄 已重启 {} 的容器（下次发消息自动启动）",
            friend.nickname.as_deref().unwrap_or("?")
        )
    }

    async fn cmd_destroy(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /destroy 昵称".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }

        let friend = &matches[0];
        let _ = self.executor.destroy_container(&friend.wxid).await;
        format!(
            "🗑️ 已销毁 {} 的容器（数据保留，下次发消息自动重建）",
            friend.nickname.as_deref().unwrap_or("?")
        )
    }

    async fn cmd_rebuild(&self, args: &str) -> String {
        if args.is_empty() {
            return "用法: /rebuild 昵称".to_string();
        }

        let matches = match self.db.friend_find_by_nickname(args.trim()) {
            Ok(m) => m,
            Err(_) => return "❌ 查询出错".to_string(),
        };
        if matches.is_empty() {
            return format!("❌ 未找到 \"{}\"", args);
        }

        let friend = &matches[0];
        let permission = parse_permission(&friend.permission);
        let _ = self.executor.rebuild_container(&friend.wxid, permission).await;
        format!("🔨 已重建 {} 的容器", friend.nickname.as_deref().unwrap_or("?"))
    }

    async fn cmd_stopall(&self) -> String {
        let containers = match self.executor.list_containers().await {
            Ok(c) => c,
            Err(_) => return "❌ 查询容器失败".to_string(),
        };

        for c in &containers {
            if let Some(ref wxid) = c.wxid {
                let _ = self.executor.stop_container(wxid).await;
            }
        }

        format!("⏹️ 已停止全部 {} 个容器", containers.len())
    }

    // ============================================
    // Security check
    // ============================================

    fn security_check(&self, message: &str, permission: &str) -> Option<String> {
        if permission == "admin" {
            return None;
        }

        let config = get_config();
        for pattern in &config.security.blocked_patterns {
            if let Ok(re) = Regex::new(&format!("(?i){}", pattern)) {
                if re.is_match(message) {
                    warn!("安全拦截: {}", &message[..message.len().min(100)]);
                    return Some("消息包含不允许的操作".to_string());
                }
            }
        }

        None
    }
}

// ============================================
// Free-standing helpers
// ============================================

fn format_logs(logs: &[AuditEntry]) -> String {
    if logs.is_empty() {
        return "暂无日志".to_string();
    }

    logs.iter()
        .map(|l| {
            let dir = if l.direction == "in" { "📩" } else { "📤" };
            let time = l
                .timestamp
                .as_deref()
                .and_then(|t| t.split(' ').nth(1))
                .unwrap_or(l.timestamp.as_deref().unwrap_or(""));
            let nickname = l.nickname.as_deref().unwrap_or("");
            let msg = l.message.as_deref().map(|m| &m[..m.len().min(60)]).unwrap_or("");
            format!("{} [{}] {}: {}", dir, time, nickname, msg)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}
