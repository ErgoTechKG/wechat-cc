# WeChat-Claude Code Bridge (Docker Sandbox)

A Rust application that bridges WeChat friends to Claude Code, giving each friend their own isolated Docker container environment. Friends chat via WeChat, and their messages are routed to Claude Code running inside per-user sandboxed containers.

## Architecture

```
 WeChat Friend A ──┐
 WeChat Friend B ──┼──▶ WeChat Bot ──▶ MessageRouter ──▶ ClaudeExecutor
 WeChat Friend C ──┘    (wechaty)         │                    │
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

 Persistent data: ~/claude-bridge-data/<wxid>/workspace/
                  ~/claude-bridge-data/<wxid>/claude-config/
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
# Edit config.yaml -- set admin_wxid at minimum

# 3. Build the sandbox Docker image
cd docker && docker compose build sandbox-base && cd ..

# 4. Run (starts in stdin test mode by default)
cargo run --release
```

## Authentication (Claude Code)

Claude Code supports two authentication modes. This bridge works with **either** -- choose whichever fits your setup.

### Option A: Claude Code Max (Subscription)

No API key needed. Claude Code authenticates via OAuth, and the session is stored in `~/.claude/` inside each container (persisted via the `claude-config/` volume mount).

**First-time setup per user**: the first time Claude runs in a new container, it needs to complete an OAuth login:

```bash
docker exec -it claude-friend-<wxid> claude --print "hello"
# Complete the OAuth flow in browser, then the session is saved
```

Subsequent calls reuse the stored session automatically.

### Option B: Anthropic API Key

Set the environment variable before starting the bridge:

```bash
export ANTHROPIC_API_KEY=sk-ant-xxx
cargo run --release
```

When set, the key is passed into every container automatically. When not set, it is simply omitted and containers use whatever auth is in their `~/.claude/` config.

## WeChat Connection

WeChat integration uses the **wechaty** ecosystem, which works by scanning a QR code with your personal WeChat account (similar to WeChat Web login). **No WeChat API key is needed.**

The app defines a pluggable `WeChatBot` trait in `src/wechat_bot.rs`. Currently it ships with `StdinBot` for local testing. To connect to real WeChat:

1. Run a **wechaty puppet service** (Node.js process) that handles the WeChat protocol
2. Implement the `WeChatBot` trait to connect to that puppet via gRPC or HTTP
3. Swap `StdinBot::new()` for your implementation in `src/main.rs`

Other WeChat bot frameworks (itchat, WeChatFerry, etc.) can also work -- just implement the trait.

### Testing with StdinBot

Without WeChat, you can test the full pipeline via stdin. Messages use the format:

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
| `admin_wxid` | `""` | WeChat ID of the admin user (**required**) |
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
    ├── wechat_bot.rs          # WeChatBot trait + StdinBot for testing
    └── error.rs               # Error types
```

## Data Persistence

Each friend's data is stored in `~/claude-bridge-data/<wxid>/`:

- `workspace/` -- the friend's working directory (code, files, etc.)
- `claude-config/` -- Claude Code auth and session cache (`~/.claude/` inside the container)

Data survives container destruction. When a container is rebuilt, volumes are re-mounted automatically.

The SQLite database (`data/bridge.db`) stores:
- Friend records and permissions
- Session tracking
- Message audit log
- Rate limit counters

## Stopping the Service

As the admin, you can control the service directly from WeChat:

| Command | What it does |
|---------|-------------|
| `/stopall` | Stop all friend containers (no one can use Claude until restart) |
| `/destroy <name>` | Remove a specific friend's container |
| `/block <name>` | Block a friend and destroy their container |
| `/kill <name>` | Kill a friend's running Claude process without stopping their container |

To fully shut down the bridge, stop the server process (`Ctrl+C` or kill the process). This disconnects the WeChat bot -- no messages will be received or processed. Containers are set to `restart: unless-stopped`, so they remain paused until the bridge starts again.

To also revoke the WeChat login from your phone: WeChat -> Settings -> Account Security -> Login Devices -> remove the bot device.

## Development

```bash
# Build
cargo build

# Run tests (16 unit tests)
cargo test

# Run with debug logging
RUST_LOG=debug cargo run

# Build sandbox Docker image manually
cd docker && docker compose build sandbox-base
```

## License

MIT
