---
name: wechat-cc-test
description: This skill should be used when testing the wechat-cc project (Telegram/WeChat Claude Code bridge in Rust). It provides a structured workflow for building, running unit tests, performing end-to-end smoke tests with real Claude Code responses, and writing new test cases. Trigger when the user asks to test, validate, debug, or add test coverage to the project.
---

# WeChat-CC Test Skill

## Overview

Test the wechat-cc project end-to-end: build, run 123+ unit tests, then perform a live smoke test that sends a real message through the bridge and verifies Claude Code responds. The project supports two frontends: **Telegram Bot** (production) and **StdinBot** (testing). Authentication is via `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) or `ANTHROPIC_API_KEY` env var, injected into all containers automatically.

**Never assume the app works. Always run the end-to-end smoke test.**

## Test Workflow (Required Steps)

Follow ALL steps in order. Do not skip the smoke test.

### Step 1: Build

```bash
cargo build --release 2>&1
```

Check for compilation errors and warnings. Fix any errors before proceeding.

### Step 2: Unit Tests

```bash
cargo test 2>&1
```

All 123+ tests must pass. If any fail, diagnose and fix before proceeding.
To run a specific module: `cargo test --lib <module>::tests` (e.g., `cargo test --lib database::tests`).

### Step 3: Prerequisites Check

Before the smoke test, verify:

```bash
# Docker must be running
docker info > /dev/null 2>&1 && echo "OK: Docker running" || echo "FAIL: Docker not running"

# Auth token must be set (CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY)
[ -n "$CLAUDE_CODE_OAUTH_TOKEN" ] && echo "OK: CLAUDE_CODE_OAUTH_TOKEN set" || \
[ -n "$ANTHROPIC_API_KEY" ] && echo "OK: ANTHROPIC_API_KEY set" || \
echo "FAIL: no auth token set. Run 'claude setup-token' to generate one"

# config.yaml must exist
ls config.yaml > /dev/null 2>&1 && echo "OK: config.yaml exists" || echo "FAIL: no config.yaml"
```

If `config.yaml` is missing, create from template:
```bash
cp config.example.yaml config.yaml
# Set admin_wxid to match the wxid used in smoke test (e.g., "admin_test")
```

If no auth token is set:
```bash
claude setup-token  # Interactive — follow prompts to get token
export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-xxx
```

### Step 4: End-to-End Smoke Test (MANDATORY)

This is the critical test. It verifies the full pipeline: message parsing → friend registration → container creation → Claude Code execution → response delivery.

**IMPORTANT:** The smoke test wxid must match `admin_wxid` in config.yaml, because admin users get bridge network (internet access needed for Claude API calls). Normal users get `none` network and Claude will fail silently.

**Run the app with a piped message (app exits on EOF):**

```bash
echo 'admin_test|Admin|Say hello in one sentence' | cargo run --release 2>&1
```

**What to check in the output:**

1. **Startup logs** — look for:
   - `Starting WeChat -> Claude Code bridge...`
   - `Docker version: X.X.X`
   - `StdinBot started`
   - `Docker environment ready`

2. **Message processing** — look for:
   - `收到消息 [Admin(admin_test)]: Say hello in one sentence`
   - `新好友注册: Admin(admin_test)` (first run only)
   - `Created container: claude-friend-admin_test` (first run only)
   - `Created new session: admin_test -> <uuid>`

3. **Claude response** — look for:
   - `[Admin] <actual response text>` — this is the reply line
   - The response must NOT be `(Claude returned no content)`
   - The response must NOT be an error message

**If Claude returns no content:**
- Check auth token is set: `echo $CLAUDE_CODE_OAUTH_TOKEN` (must not be empty)
- Check the wxid matches `admin_wxid` in config.yaml (admin gets bridge network with internet)
- Check container network: `docker inspect claude-friend-admin_test --format '{{.HostConfig.NetworkMode}}'` (must be `bridge`, not `none`)
- Normal users get `none` network (no internet) and Claude will silently fail
- Verify token works on host: `CLAUDE_CODE_OAUTH_TOKEN=xxx claude --print "hello"`

**If the container fails to start:**
- Check Docker is running: `docker info`
- Check sandbox image exists: `docker images claude-sandbox:latest`
- If missing, it auto-builds on first run (takes ~1 minute)

### Step 5: Telegram Bot Smoke Test (Optional)

If `telegram.enabled: true` and `telegram.bot_token` is set in config.yaml, test the Telegram integration:

```bash
# Ensure ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN is exported
source .env && export ANTHROPIC_API_KEY
cargo run --release 2>&1
```

Then send messages to the bot on Telegram:
- Send "Hello" — should get a Claude response
- Send "/help" — should list available commands
- Send "/status" — should show container info

**Key checks:**
- Log shows `Using Telegram bot` and `Telegram bot online: @your_bot_name`
- Your Telegram chat ID must match `admin_wxid` in config.yaml for admin privileges
- Admin user gets `bridge` network (internet); normal gets `none` (Claude fails silently)

### Step 5b: StdinBot Smoke Test (Alternative)

With `telegram.enabled: false`, test via stdin pipe:

```bash
RUST_LOG=info cargo run --release
```

Then type test messages:
```
test_admin|Admin|/help
test_admin|Admin|/status
test_admin|Admin|What is 2+2?
test_user|Alice|Hello
test_admin|Admin|/list
test_admin|Admin|/containers
```

Verify each command returns the expected response. Set `admin_wxid` in `config.yaml` to match the wxid you use (e.g., `test_admin`).

### Step 6: Cleanup

After testing, clean up containers:

```bash
docker rm -f $(docker ps -a --filter "label=app=wechat-claude-bridge" -q) 2>/dev/null
```

## Smoke Test Success Criteria

The test passes ONLY when ALL of these are true:

- [ ] `cargo build --release` compiles with zero errors
- [ ] `cargo test` — all 123+ tests pass
- [ ] Docker is running and sandbox image is built
- [ ] Piped message produces a real Claude response (not empty, not error)
- [ ] Response is printed as `[Nickname] <response text>`

If any criterion fails, the test fails. Diagnose and fix before declaring success.

## Test Modules & Coverage

The unit test suite covers 6 modules. Read `references/test_map.md` for the full test inventory.

| Module | Tests | Key Areas |
|--------|-------|-----------|
| `main.rs` (tests) | ~22 | Memory parsing, CPU conversion, message splitting |
| `config.rs` | ~21 | Default values, YAML deserialization, path expansion |
| `database.rs` | ~26 | Friends CRUD, sessions, rate limits, audit log |
| `docker_manager.rs` | ~13 | Container naming, permissions, config defaults |
| `message_router.rs` | ~22 | Display names, log formatting, byte formatting, permission levels |
| `claude_executor.rs` | ~11 | Permission parsing, session expiry, truncation |

## Common Bugs Found

When writing or reviewing code, watch for these patterns (documented in `references/common_bugs.md`):

### 1. UTF-8 Truncation (Critical)
Truncating a Rust `String` by byte index without checking `is_char_boundary()` panics on multi-byte characters (Chinese, emoji). Always use char-boundary-safe truncation.

### 2. Message Splitting Edge Cases
`split_message()` must handle: empty strings, strings exactly at limit, all-newline content, emoji sequences, Chinese text where each char is 3 bytes.

### 3. Rate Limiting Boundaries
- Zero limits (0 per minute / 0 per day) should block all requests
- Independent per-user tracking must not leak between users

### 4. Session Expiry
- Future timestamps should NOT be treated as expired
- Invalid date formats should be treated as expired (safe default)
- Only `%Y-%m-%d %H:%M:%S` format is supported (not ISO 8601 with `T`)

### 5. Negative Duration Casting
Casting a negative `i64` to `u64` wraps to a huge number. Always check sign before casting `elapsed.num_minutes()` in expiry logic.

### 6. Auth / Empty Response
If Claude returns no content, the most common causes are:
- **Missing auth token**: `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` not set
- **Wrong user for smoke test**: Normal users get `none` network (no internet). Use the admin wxid for smoke tests
- **Token expired**: Re-run `claude setup-token` on the host
Verify with `CLAUDE_CODE_OAUTH_TOKEN=xxx claude --print "hello"` on the host.

## Writing New Test Cases

Each source file has a `#[cfg(test)] mod tests` block at the bottom. Add new tests there.

### Test Naming Convention
```rust
#[test]
fn <function_or_area>_<scenario>() { ... }
```

### Priority Areas for New Tests
1. **Security patterns** — `blocked_patterns` regex matching (e.g., `rm -rf /`, fork bombs)
2. **Command parsing** — `/allow`, `/block` with edge-case inputs
3. **Concurrency guards** — one-request-per-user logic
4. **Docker error paths** — container not found, image missing, timeout
5. **Config validation** — invalid YAML, missing required fields

For database tests, use in-memory SQLite: `Database::new(":memory:").unwrap()`

## Analyzing Test Results

1. **Read the failure output** — Rust shows left/right values on assertion failures
2. **Check for UTF-8 issues** — Panics mentioning "byte index" or "char boundary"
3. **Check for race conditions** — Timestamp-based tests may be flaky
4. **Run single test** for faster iteration: `cargo test <test_name> -- --nocapture`

## Resources

### references/
- `test_map.md` - Complete inventory of all 123 tests by module
- `common_bugs.md` - Detailed bug patterns with reproduction steps and fixes
