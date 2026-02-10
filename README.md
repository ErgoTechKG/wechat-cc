# WeChat-Claude Code Bridge (Docker Sandbox)

A Rust application that bridges messaging platforms to Claude Code, giving each user their own isolated Docker container environment. Users chat via **Telegram** (or stdin for testing), and their messages are routed to Claude Code running inside per-user sandboxed containers.

## Architecture

```
 Telegram User A ──┐
 Telegram User B ──┼──▶ TelegramBot ──▶ MessageRouter ──▶ ClaudeExecutor
 Telegram User C ──┘   (Bot API)            │                    │
                                            │              DockerManager
                                         SQLite                  │
                                       friends/           ┌──────┴───────────────────┐
                                       sessions/          │  ┌────────────────────┐  │
                                       audit log          │  │ Container A        │  │
                                                          │  │ 512M / 1CPU / none │  │
                                                          │  └────────────────────┘  │
                                                          │  ┌────────────────────┐  │
                                                          │  │ Container B        │  │
                                                          │  │ 512M / 1CPU / limited│ │
                                                          │  └────────────────────┘  │
                                                          │  ┌────────────────────┐  │
                                                          │  │ Container C (admin)│  │
                                                          │  │ 2G / 2CPU / bridge │  │
                                                          │  └────────────────────┘  │
                                                          └──────────────────────────┘

 Persistent data: ~/claude-bridge-data/<user_id>/workspace/
 Auth:            CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY (env var → all containers)
```

## Prerequisites

- **Rust** 1.70+ (with cargo)
- **Docker** installed and running
- **Claude Code CLI** installed in the sandbox image (already handled by `docker/Dockerfile.sandbox`)

## Quick Start

```bash
# 1. Build
git clone <repo> && cd wechat-cc
cargo build --release

# 2. Configure
cp config.example.yaml config.yaml
# Edit config.yaml:
#   - Set admin_wxid to your Telegram chat ID
#   - Set telegram.enabled: true
#   - Set telegram.bot_token (from @BotFather)

# 3. Build the sandbox Docker image
cd docker && docker compose build sandbox-base && cd ..

# 4. Run
export ANTHROPIC_API_KEY=sk-ant-xxx  # or CLAUDE_CODE_OAUTH_TOKEN
cargo run --release
```

## Authentication (Claude Code)

Authentication is passed to all containers via environment variables. Set up once on the host, and every container inherits the credentials automatically.

### Option A: Claude Code Max (Subscription) -- Recommended

Generate a long-lived OAuth token on the host:

```bash
claude setup-token
# Follow the interactive prompts, then copy the token
```

Start the bridge with the token:

```bash
export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-xxx
cargo run --release
```

The token is injected into every container. No per-user login needed.

### Option B: Anthropic API Key

```bash
export ANTHROPIC_API_KEY=sk-ant-xxx
cargo run --release
```

When set, the key is passed into every container automatically.

## Telegram Bot Setup

1. Open Telegram, find **@BotFather**, send `/newbot`
2. Choose a name and username for your bot
3. Copy the bot token (e.g., `123456789:ABCdefGHIjklMNOpqrsTUVwxyz`)
4. Set in `config.yaml`:
   ```yaml
   telegram:
     enabled: true
     bot_token: "YOUR_TOKEN_HERE"
   ```
5. Send any message to your bot, check logs for your chat ID, set it as `admin_wxid`

The bot uses long-polling (`getUpdates`) — no webhook or public URL needed. Only private text messages are processed; group messages are ignored.

### Pluggable Bot Interface

The app defines a `WeChatBot` trait in `src/wechat_bot.rs`. Two implementations ship:

- **TelegramBot** (`src/telegram_bot.rs`) — Telegram Bot API via long-polling (production)
- **StdinBot** (`src/wechat_bot.rs`) — stdin pipe for local testing

Set `telegram.enabled: false` (or omit) to use StdinBot. Messages use the format:

```
wxid|nickname|message text
```

Example session:

```
wxid_admin|Admin|/help
wxid_admin|Admin|/list
wxid_user1|Alice|Hello, explain quicksort
wxid_admin|Admin|/containers
wxid_admin|Admin|/allow Alice trusted
```

## Configuration

All settings are in `config.yaml`. See [`config.example.yaml`](config.example.yaml) for the full template with comments.

### Key Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `admin_wxid` | `""` | Admin user ID — Telegram chat ID or WeChat wxid (**required**) |
| `telegram.enabled` | `false` | Enable Telegram bot (otherwise uses StdinBot) |
| `telegram.bot_token` | `""` | Telegram bot token from @BotFather |
| `claude.timeout` | `120` | Seconds before Claude execution times out |
| `docker.image` | `claude-sandbox:latest` | Docker image for sandbox containers |
| `docker.data_dir` | `~/claude-bridge-data` | Persistent data root (each user gets a subdirectory) |
| `docker.limits.memory` | `512m` | Memory limit for normal/trusted users |
| `docker.limits.admin_memory` | `2g` | Memory limit for admin |
| `rate_limit.max_per_minute` | `10` | Max messages per user per minute |
| `rate_limit.max_per_day` | `200` | Max messages per user per day |
| `permissions.default_level` | `normal` | Default permission for new friends |

## Permission Levels

| Level | Container | Network | Capabilities |
|-------|-----------|---------|-------------|
| **admin** | 2G memory, 2 CPU | Full (bridge) | Everything + management commands |
| **trusted** | 512M memory, 1 CPU | Limited (API only) | Code execution, file operations |
| **normal** | 512M memory, 1 CPU | None (offline) | Q&A only |
| **blocked** | No container | -- | Ignored |

## Commands

### Available to All Users

| Command | Description |
|---------|-------------|
| *(text)* | Chat with Claude |
| `/help` | Show available commands |
| `/status` | Show status (container resources) |
| `/clear` | Clear conversation history |

### Admin Only

| Command | Description |
|---------|-------------|
| `/allow <name> [level]` | Authorize a friend (`trusted`, `normal`, or `admin`) |
| `/block <name>` | Block a friend (destroys their container) |
| `/list` | List all authorized friends |
| `/logs [name]` | View audit logs |
| `/kill <name>` | Kill a friend's running Claude process |
| `/containers` | List all containers and their status |
| `/restart <name>` | Restart a friend's container |
| `/destroy <name>` | Destroy container (data preserved) |
| `/rebuild <name>` | Rebuild container (after image updates) |
| `/stopall` | Stop all containers |

## Isolation Strategy

| Dimension | Implementation |
|-----------|---------------|
| **Process** | Each friend gets a dedicated Docker container (PID namespace) |
| **Filesystem** | Read-only rootfs + isolated workspace volume per user |
| **Network** | `normal` = no network, `trusted` = limited, `admin` = full |
| **Resources** | Memory 512M, CPU 1 core, max 100 PIDs per container |
| **Privileges** | Non-root user inside container, all capabilities dropped |
| **Hardening** | `no-new-privileges`, read-only root filesystem |

## Project Structure

```
wechat-cc/
├── Cargo.toml                 # Rust dependencies
├── config.example.yaml        # Configuration template
├── docker/
│   ├── Dockerfile.sandbox     # Sandbox container image
│   └── docker-compose.yaml    # Image build helper
└── src/
    ├── main.rs                # Entry point, startup sequence, message loop
    ├── config.rs              # YAML config loading (serde + OnceLock)
    ├── database.rs            # SQLite: friends, sessions, audit, rate limits
    ├── docker_manager.rs      # Container lifecycle via bollard (Docker API)
    ├── claude_executor.rs     # Claude Code execution in containers
    ├── message_router.rs      # Message routing + 14 commands
    ├── telegram_bot.rs        # Telegram Bot API (long-polling)
    ├── wechat_bot.rs          # WeChatBot trait + StdinBot for testing
    └── error.rs               # Error types
```

## Data Persistence

Each friend's data is stored in `~/claude-bridge-data/<wxid>/workspace/` (code, files, etc.).

Authentication is passed via environment variables (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`), injected into every container at startup. No per-user auth data is stored.

Data survives container destruction. When a container is rebuilt, volumes are re-mounted automatically.

The SQLite database (`data/bridge.db`) stores:
- Friend records and permissions
- Session tracking
- Message audit log
- Rate limit counters

## Stopping the Service

As the admin, you can control the service directly from Telegram (or stdin):

| Command | What it does |
|---------|-------------|
| `/stopall` | Stop all friend containers (no one can use Claude until restart) |
| `/destroy <name>` | Remove a specific friend's container |
| `/block <name>` | Block a friend and destroy their container |
| `/kill <name>` | Kill a friend's running Claude process without stopping their container |

To fully shut down the bridge, stop the server process (`Ctrl+C` or kill the process). This disconnects the bot -- no messages will be received or processed. Containers are set to `restart: unless-stopped`, so they remain paused until the bridge starts again.

## Development

```bash
# Build
cargo build

# Run tests (123 unit tests)
cargo test

# Run with debug logging
RUST_LOG=debug cargo run

# Build sandbox Docker image manually
cd docker && docker compose build sandbox-base
```

## License

MIT
