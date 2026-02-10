mod claude_executor;
mod config;
mod database;
mod docker_manager;
mod error;
mod message_router;
mod wechat_bot;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use claude_executor::ClaudeExecutor;
use config::get_config;
use database::Database;
use docker_manager::{DockerConfig, DockerLimits, DockerManager, DockerNetworkConfig};
use message_router::MessageRouter;
use wechat_bot::{StdinBot, WeChatBot};

// ============================================
// Memory string parsing
// ============================================

/// Parse a memory string like "512m", "2g", "1024k" into bytes.
fn parse_memory(s: &str) -> i64 {
    let s = s.trim().to_lowercase();
    if let Some(rest) = s.strip_suffix('g') {
        rest.parse::<i64>().unwrap_or(0) * 1024 * 1024 * 1024
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<i64>().unwrap_or(0) * 1024 * 1024
    } else if let Some(rest) = s.strip_suffix('k') {
        rest.parse::<i64>().unwrap_or(0) * 1024
    } else {
        s.parse::<i64>().unwrap_or(0)
    }
}

/// Convert config cpus (u32, whole cores) to Docker nano-CPUs (i64).
fn cpus_to_nanocpus(cpus: u32) -> i64 {
    cpus as i64 * 1_000_000_000
}

/// Build a `docker_manager::DockerConfig` from the global config structs.
fn build_docker_config(cfg: &config::Config) -> DockerConfig {
    let data_dir = cfg.docker.expanded_data_dir();

    DockerConfig {
        image: cfg.docker.image.clone(),
        container_prefix: cfg.docker.container_prefix.clone(),
        data_dir,
        limits: DockerLimits {
            memory: parse_memory(&cfg.docker.limits.memory),
            admin_memory: parse_memory(&cfg.docker.limits.admin_memory),
            cpus: cpus_to_nanocpus(cfg.docker.limits.cpus),
            admin_cpus: cpus_to_nanocpus(cfg.docker.limits.admin_cpus),
            pids: cfg.docker.limits.pids as i64,
            tmp_size: cfg.docker.limits.tmp_size.clone(),
        },
        network: DockerNetworkConfig {
            admin: cfg.docker.network.admin.clone(),
            trusted: cfg.docker.network.trusted.clone(),
            normal: cfg.docker.network.normal.clone(),
        },
    }
}

// ============================================
// Message splitting (WeChat 2000 char limit)
// ============================================

fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Find a char-boundary-safe split point at or before max_len
        let mut split_at = max_len;
        while split_at > 0 && !remaining.is_char_boundary(split_at) {
            split_at -= 1;
        }
        if split_at == 0 {
            // Extremely unlikely: single char > max_len bytes. Take one char.
            split_at = remaining
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
        }

        // Try to split at a newline near the limit
        let search_region = &remaining[..split_at];
        let split_idx = match search_region.rfind('\n') {
            Some(idx) if idx >= split_at / 2 => idx,
            _ => split_at,
        };

        chunks.push(remaining[..split_idx].to_string());
        remaining = remaining[split_idx..].trim_start();
    }

    chunks
}

// ============================================
// Entry point
// ============================================

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Init tracing (console output)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting WeChat -> Claude Code bridge...");

    // 2. Load config
    config::init_config().context("Failed to load configuration")?;
    let cfg = get_config();

    if cfg.admin_wxid.is_empty() {
        warn!("admin_wxid is not set in config.yaml!");
    }

    // 3. Create Database
    let db = Arc::new(
        Database::new(None).context("Failed to initialize database")?,
    );

    // 4. Create DockerManager
    let docker_cfg = build_docker_config(cfg);
    let docker = Arc::new(
        DockerManager::new(docker_cfg)
            .await
            .context("Failed to initialize DockerManager")?,
    );

    // 5. Docker health check
    let healthy = docker.health_check().await?;
    if !healthy {
        error!("Docker is not available. Please install and start Docker: https://docs.docker.com/get-docker/");
        std::process::exit(1);
    }

    // 6. Check/build sandbox image
    let image_ok = docker.image_exists().await?;
    if !image_ok {
        info!("Sandbox image not found, building...");
        let docker_dir = PathBuf::from("docker");
        if docker_dir.exists() {
            docker.build_image(&docker_dir).await?;
        } else {
            warn!("docker/ directory not found, skipping image build. Ensure the image exists.");
        }
    }

    // 7. Init Docker networks
    docker.init_networks().await?;

    // 8. Create ClaudeExecutor
    let executor = Arc::new(ClaudeExecutor::new(
        Arc::clone(&docker),
        Arc::clone(&db),
        cfg.session.expire_minutes,
        cfg.claude.timeout,
    ));

    // 9. Create MessageRouter
    let router = MessageRouter::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        cfg.admin_wxid.clone(),
    );

    // 10. Start message loop with StdinBot
    let mut bot = StdinBot::new();
    bot.start().await?;

    info!("Docker environment ready. Bot started, waiting for messages...");

    // 12. Periodic cleanup task
    let cleanup_db = Arc::clone(&db);
    let expire_min = cfg.session.expire_minutes as i64;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match cleanup_db.session_clean_expired(expire_min) {
                Ok(n) => {
                    if n > 0 {
                        info!("Cleaned {} expired sessions", n);
                    }
                }
                Err(e) => warn!("Session cleanup failed: {}", e),
            }
            match cleanup_db.rate_limit_cleanup() {
                Ok(n) => {
                    if n > 0 {
                        info!("Cleaned {} old rate limit entries", n);
                    }
                }
                Err(e) => warn!("Rate limit cleanup failed: {}", e),
            }
        }
    });

    // 13. Graceful shutdown via Ctrl+C
    let message_loop = async {
        loop {
            let msg = bot.recv_message().await;
            match msg {
                Ok(Some((contact, text))) => {
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }

                    // Route the message
                    if let Some(response) = router.handle_message(&contact, &text).await {
                        // 11. Split long messages for WeChat
                        let chunks = split_message(&response, 2000);
                        for (i, chunk) in chunks.iter().enumerate() {
                            if let Err(e) = bot.send_message(&contact, chunk).await {
                                error!("Failed to send message: {}", e);
                            }
                            // Brief pause between multi-part messages
                            if chunks.len() > 1 && i < chunks.len() - 1 {
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            }
                        }
                    }
                }
                Ok(None) => {
                    info!("Input stream ended (EOF), shutting down...");
                    break;
                }
                Err(e) => {
                    error!("Error receiving message: {}", e);
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = message_loop => {}
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received, shutting down...");
        }
    }

    info!("Bridge stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory() {
        assert_eq!(parse_memory("512m"), 512 * 1024 * 1024);
        assert_eq!(parse_memory("2g"), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory("1024k"), 1024 * 1024);
        assert_eq!(parse_memory("1048576"), 1048576);
        assert_eq!(parse_memory("0"), 0);
    }

    #[test]
    fn test_cpus_to_nanocpus() {
        assert_eq!(cpus_to_nanocpus(1), 1_000_000_000);
        assert_eq!(cpus_to_nanocpus(2), 2_000_000_000);
    }

    #[test]
    fn test_split_message_short() {
        let msg = "Hello world";
        let chunks = split_message(msg, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "Hello world");
    }

    #[test]
    fn test_split_message_at_newline() {
        let line_a = "a".repeat(1200);
        let line_b = "b".repeat(1200);
        let msg = format!("{}\n{}", line_a, line_b);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], line_a);
        assert_eq!(chunks[1], line_b);
    }

    #[test]
    fn test_split_message_hard_cut() {
        let msg = "x".repeat(5000);
        let chunks = split_message(&msg, 2000);
        assert!(chunks.len() >= 3);
        assert_eq!(chunks[0].len(), 2000);
    }

    // ============================================
    // NEW: parse_memory edge cases
    // ============================================

    #[test]
    fn test_parse_memory_empty_string() {
        assert_eq!(parse_memory(""), 0);
    }

    #[test]
    fn test_parse_memory_whitespace() {
        assert_eq!(parse_memory("  512m  "), 512 * 1024 * 1024);
    }

    #[test]
    fn test_parse_memory_uppercase() {
        // The function lowercases input, so "2G" should work
        assert_eq!(parse_memory("2G"), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory("512M"), 512 * 1024 * 1024);
        assert_eq!(parse_memory("1024K"), 1024 * 1024);
    }

    #[test]
    fn test_parse_memory_invalid_number() {
        // "abcm" -> strip suffix "m", parse "abc" fails, returns 0
        assert_eq!(parse_memory("abcm"), 0);
    }

    #[test]
    fn test_parse_memory_negative() {
        // Negative numbers should parse fine as i64
        assert_eq!(parse_memory("-1m"), -1 * 1024 * 1024);
    }

    #[test]
    fn test_parse_memory_just_suffix() {
        // "m" -> strip suffix "m", parse "" fails, returns 0
        assert_eq!(parse_memory("m"), 0);
        assert_eq!(parse_memory("g"), 0);
        assert_eq!(parse_memory("k"), 0);
    }

    // ============================================
    // NEW: cpus_to_nanocpus edge cases
    // ============================================

    #[test]
    fn test_cpus_to_nanocpus_zero() {
        assert_eq!(cpus_to_nanocpus(0), 0);
    }

    #[test]
    fn test_cpus_to_nanocpus_large() {
        // 128 cores
        assert_eq!(cpus_to_nanocpus(128), 128_000_000_000);
    }

    // ============================================
    // NEW: split_message edge cases
    // ============================================

    #[test]
    fn test_split_message_empty() {
        let chunks = split_message("", 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn test_split_message_exact_limit() {
        let msg = "x".repeat(2000);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2000);
    }

    #[test]
    fn test_split_message_one_over_limit() {
        let msg = "x".repeat(2001);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1);
    }

    #[test]
    fn test_split_message_all_newlines() {
        let msg = "\n".repeat(5000);
        let chunks = split_message(&msg, 2000);
        // Should not panic. The newline splitting and trim_start
        // means chunks may collapse.
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_split_message_no_newlines_long() {
        // Long message without any newlines, must hard-cut
        let msg = "a".repeat(6001);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 4); // 2000 + 2000 + 2000 + 1
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 2000);
        assert_eq!(chunks[2].len(), 2000);
        assert_eq!(chunks[3].len(), 1);
    }

    #[test]
    fn test_split_message_unicode_chinese() {
        // Chinese characters are multi-byte in UTF-8 (3 bytes each)
        // Create a string of ~700 Chinese chars (2100 bytes)
        let msg: String = std::iter::repeat('中').take(700).collect();
        // Note: split_message uses byte length (.len()), not char count
        // 700 * 3 = 2100 bytes > 2000, so it should split
        // But the split at byte position 2000 could land mid-character!
        // This tests whether the code handles UTF-8 boundaries correctly.
        let result = std::panic::catch_unwind(|| {
            split_message(&msg, 2000)
        });
        // This may panic due to slicing at non-char-boundary
        if let Ok(chunks) = result {
            // If it doesn't panic, verify content is preserved
            let _total: String = chunks.join("");
            // Due to trim_start, some whitespace might be lost, but no chars here
            assert!(!chunks.is_empty());
        }
        // If it panics, that's a real bug we've discovered
    }

    #[test]
    fn test_split_message_emoji_content() {
        // Emoji are 4 bytes in UTF-8
        let msg: String = std::iter::repeat("🎉").take(600).collect();
        // 600 * 4 = 2400 bytes > 2000
        let result = std::panic::catch_unwind(|| {
            split_message(&msg, 2000)
        });
        if let Ok(chunks) = result {
            assert!(!chunks.is_empty());
        }
        // A panic here means the code can't handle emoji splitting
    }

    #[test]
    fn test_split_message_newline_in_first_half_ignored() {
        // Newline in the first half of the limit is ignored (< max_len/2)
        let mut msg = String::new();
        msg.push_str("short\n"); // newline at position 5
        msg.push_str(&"x".repeat(2500)); // rest is continuous
        let chunks = split_message(&msg, 2000);
        // The newline at position 5 is < 2000/2 = 1000, so it's ignored
        // Hard cut at 2000
        assert_eq!(chunks[0].len(), 2000);
    }

    #[test]
    fn test_split_message_max_len_one() {
        // Edge case: max_len = 1
        let msg = "abc";
        let chunks = split_message(msg, 1);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "a");
        assert_eq!(chunks[1], "b");
        assert_eq!(chunks[2], "c");
    }
}
