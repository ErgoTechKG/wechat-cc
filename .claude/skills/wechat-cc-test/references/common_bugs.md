# Common Bugs in wechat-cc

## Bug #1: UTF-8 Truncation Panic

**Severity:** Critical (causes panic/crash)
**Affected files:** `claude_executor.rs`, `main.rs` (any code using `String::truncate` or byte slicing)

**Root cause:** Rust strings are UTF-8 encoded. Chinese characters are 3 bytes, emoji are 4 bytes. Truncating at an arbitrary byte index that falls in the middle of a multi-byte character causes a panic.

**Symptom:** `panic: byte index N is not a char boundary; it is inside 'X'`

**Fix pattern:**
```rust
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
```

**Test cases that catch this:**
- `test_split_message_unicode_chinese`
- `test_split_message_emoji_content`

---

## Bug #2: Negative Duration Cast to u64

**Severity:** High (incorrect session behavior)
**Affected files:** `claude_executor.rs` (`is_session_expired`)

**Root cause:** When `last_active` is a future timestamp, `elapsed.num_minutes()` returns a negative `i64`. Casting negative `i64` to `u64` wraps to a huge number (e.g., -5 becomes 18446744073709551611), which is always greater than `expire_minutes`, falsely marking the session as expired.

**Fix:** Check sign before casting:
```rust
let minutes = elapsed.num_minutes();
if minutes < 0 {
    return false; // Future timestamp = not expired
}
minutes as u64 > expire_minutes
```

**Test case:** `session_expired_future_timestamp`

---

## Bug #3: SQL Wildcard Injection in Nickname Search

**Severity:** Medium (data leak, not security critical)
**Affected files:** `database.rs` (`find_friend_by_nickname`)

**Root cause:** If a user's search string contains SQL `%` or `_` wildcards, they are interpreted as LIKE pattern characters rather than literal characters.

**Test case:** `friend_find_by_nickname_sql_wildcard`

---

## Bug #4: Empty wxid Handling

**Severity:** Low (edge case)
**Affected files:** `docker_manager.rs` (`container_name`)

**Root cause:** An empty wxid produces a container name with no distinguishing suffix, potentially causing container name collisions.

**Test case:** `test_container_name_empty_wxid`

---

## Patterns to Watch For

### When modifying string truncation:
- Always test with Chinese text (3-byte chars) and emoji (4-byte chars)
- Use `is_char_boundary()` before any byte-level truncation
- Test with strings exactly at, one under, and one over the limit

### When modifying time-based logic:
- Test with past, present, future, and invalid timestamps
- Never cast signed duration to unsigned without checking sign
- Test boundary values (0 minutes, u64::MAX)

### When modifying database queries with user input:
- Test with SQL special characters: `%`, `_`, `'`, `"`
- Use parameterized queries (already done via rusqlite)
- Test with empty strings and very long strings

### When modifying Docker container names:
- Test with Unicode wxids (Chinese, emoji, special chars)
- Test with empty and very long wxids
- Verify the name is valid for Docker (alphanumeric, dots, hyphens)
