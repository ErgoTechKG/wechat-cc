---
name: wechat-cc-test
description: This skill should be used when testing the wechat-cc project (WeChat Claude Code bridge in Rust). It provides a structured workflow for building, running tests, analyzing coverage, identifying common bugs, and writing new test cases. Trigger when the user asks to test, validate, debug, or add test coverage to the project.
---

# WeChat-CC Test Skill

## Overview

Run and validate the wechat-cc test suite (123+ Rust unit tests), identify coverage gaps, and write new test cases. The project is a Rust application bridging WeChat to Claude Code via isolated Docker containers.

## Quick Start

To run the full test workflow:

```bash
# Build the project (check for warnings)
cargo build 2>&1

# Run all tests
cargo test 2>&1

# Run tests for a specific module
cargo test --lib config::tests
cargo test --lib database::tests
cargo test --lib docker_manager::tests
cargo test --lib message_router::tests
cargo test --lib claude_executor::tests
cargo test --lib tests  # main.rs utility tests
```

## Test Modules & Coverage

The test suite covers 6 modules. Read `references/test_map.md` for the full test inventory.

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

**Bad:**
```rust
response.truncate(MAX_LEN); // PANIC on multi-byte chars
```

**Good:**
```rust
let mut end = MAX_LEN;
while end > 0 && !s.is_char_boundary(end) {
    end -= 1;
}
response.truncate(end);
```

### 2. Message Splitting Edge Cases
`split_message()` must handle: empty strings, strings exactly at limit, all-newline content, emoji sequences, Chinese text where each char is 3 bytes.

### 3. Rate Limiting Boundaries
- Zero limits (0 per minute / 0 per day) should block all requests
- Independent per-user tracking must not leak between users
- Daily boundary transitions need correct window handling

### 4. Session Expiry
- Future timestamps should NOT be treated as expired
- Invalid date formats should be treated as expired (safe default)
- Zero-minute windows: `0 > 0` is false, so technically "not expired"
- Only `%Y-%m-%d %H:%M:%S` format is supported (not ISO 8601 with `T`)

### 5. Negative Duration Casting
Casting a negative `i64` to `u64` wraps to a huge number. Always check sign before casting `elapsed.num_minutes()` in expiry logic.

## Writing New Test Cases

When adding tests, follow these guidelines:

### Where to Add Tests
Each source file has a `#[cfg(test)] mod tests` block at the bottom. Add new tests there.

### Test Naming Convention
```rust
#[test]
fn <function_or_area>_<scenario>() { ... }
```
Examples: `session_expired_future_timestamp`, `friend_emoji_nickname`, `rate_limit_zero_limits`

### Priority Areas for New Tests
1. **Security patterns** - Test `blocked_patterns` regex matching in `message_router.rs` (e.g., `rm -rf /`, fork bombs)
2. **Command parsing** - Test `/allow`, `/block` with edge-case inputs (missing args, unicode names)
3. **Concurrency guards** - Test one-request-per-user logic in `claude_executor.rs`
4. **Docker error paths** - Container not found, image missing, timeout scenarios
5. **Config validation** - Invalid YAML, missing required fields, out-of-range values

### Test Template
```rust
#[test]
fn descriptive_test_name() {
    // Arrange
    let input = "test data";

    // Act
    let result = function_under_test(input);

    // Assert
    assert_eq!(result, expected);
}
```

For database tests, use an in-memory SQLite database:
```rust
let db = Database::new(":memory:").unwrap();
```

## Analyzing Test Results

When tests fail, follow this diagnostic workflow:

1. **Read the failure output** - Rust test output shows the assertion that failed with left/right values
2. **Check for UTF-8 issues** - Panics mentioning "byte index" or "char boundary" indicate truncation bugs
3. **Check for race conditions** - Timestamp-based tests may be flaky; use deterministic values when possible
4. **Check config defaults** - If a default value changed, multiple tests may fail simultaneously
5. **Run single test** for faster iteration: `cargo test <test_name> -- --nocapture`

## Resources

### references/
- `test_map.md` - Complete inventory of all 123 tests by module
- `common_bugs.md` - Detailed bug patterns with reproduction steps and fixes
