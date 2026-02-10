use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{info, warn};

/// A WeChat contact (sender of a message).
#[derive(Debug, Clone)]
pub struct Contact {
    pub wxid: String,
    pub nickname: String,
    pub remark_name: String,
}

/// Trait abstracting a WeChat bot. Implementations can be the real WeChat
/// puppet or a testing stub that reads from stdin.
#[async_trait]
pub trait WeChatBot: Send + Sync {
    /// Perform any startup/login sequence.
    async fn start(&mut self) -> Result<()>;

    /// Wait for and return the next incoming message.
    /// Returns `None` when the input stream is exhausted (EOF / shutdown).
    async fn recv_message(&mut self) -> Result<Option<(Contact, String)>>;

    /// Send a reply to the given contact.
    async fn send_message(&self, contact: &Contact, message: &str) -> Result<()>;
}

/// A testing bot that reads from stdin and writes to stdout.
///
/// Input format (one message per line):
///   wxid|nickname|message text
///
/// If only one `|` is present the nickname defaults to the wxid.
pub struct StdinBot {
    reader: BufReader<io::Stdin>,
}

impl StdinBot {
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(io::stdin()),
        }
    }
}

#[async_trait]
impl WeChatBot for StdinBot {
    async fn start(&mut self) -> Result<()> {
        info!("StdinBot started -- enter messages as: wxid|nickname|message");
        Ok(())
    }

    async fn recv_message(&mut self) -> Result<Option<(Contact, String)>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF
            return Ok(None);
        }

        let line = line.trim_end();
        if line.is_empty() {
            return Ok(None);
        }

        // Parse "wxid|nickname|message" or "wxid|message"
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        let (wxid, nickname, message) = match parts.len() {
            3 => (
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
            ),
            2 => (
                parts[0].to_string(),
                parts[0].to_string(),
                parts[1].to_string(),
            ),
            _ => {
                warn!("Invalid input format, expected wxid|nickname|message: {}", line);
                return Ok(None);
            }
        };

        let contact = Contact {
            wxid,
            nickname,
            remark_name: String::new(),
        };

        Ok(Some((contact, message)))
    }

    async fn send_message(&self, contact: &Contact, message: &str) -> Result<()> {
        println!("[{}] {}", contact.nickname, message);
        Ok(())
    }
}
