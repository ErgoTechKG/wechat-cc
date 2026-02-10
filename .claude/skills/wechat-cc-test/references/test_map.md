# Test Map: wechat-cc (123 tests)

## main.rs — Utility Tests (`tests::`)

### Memory Parsing (`parse_memory`)
- `test_parse_memory` — Standard suffixes: 512m, 2g, 1024k
- `test_parse_memory_empty_string` — Empty input
- `test_parse_memory_invalid_number` — Non-numeric input
- `test_parse_memory_just_suffix` — Suffix without number
- `test_parse_memory_negative` — Negative values
- `test_parse_memory_uppercase` — Case insensitivity (512M, 2G)
- `test_parse_memory_whitespace` — Leading/trailing whitespace

### CPU Conversion (`cpus_to_nanocpus`)
- `test_cpus_to_nanocpus` — Standard conversion (1.0, 0.5)
- `test_cpus_to_nanocpus_large` — Large values
- `test_cpus_to_nanocpus_zero` — Zero CPUs

### Message Splitting (`split_message`)
- `test_split_message_short` — Below limit (no split)
- `test_split_message_at_newline` — Split at newline boundary
- `test_split_message_hard_cut` — No newline, hard byte cut
- `test_split_message_empty` — Empty string
- `test_split_message_exact_limit` — Exactly at limit
- `test_split_message_one_over_limit` — One byte over
- `test_split_message_max_len_one` — Limit of 1
- `test_split_message_no_newlines_long` — Long string with no newlines
- `test_split_message_newline_in_first_half_ignored` — Newline in first half only
- `test_split_message_all_newlines` — String of only newlines
- `test_split_message_unicode_chinese` — Chinese characters (3 bytes each)
- `test_split_message_emoji_content` — Emoji sequences (4 bytes each)

## config.rs — Configuration Tests (`config::tests::`)

### Default Values
- `config_default_admin_wxid_empty` — Admin wxid defaults to empty
- `config_default_claude_cli_path` — CLI path defaults to "claude"
- `config_default_claude_timeout` — Timeout defaults to 120s
- `config_default_session_expire_minutes` — Session expires in 60min
- `config_default_session_max_history` — Max history defaults to 50
- `config_default_docker_image` — Image defaults to "claude-sandbox:latest"
- `config_default_docker_limits` — Memory 512m, CPU 1
- `config_default_docker_network` — Network defaults per permission
- `config_default_rate_limits` — 10/min, 200/day
- `config_default_permissions` — Default level is "normal"
- `config_default_security_no_blocked_patterns` — Empty by default
- `config_default_logging` — Logging defaults to "info"

### YAML Deserialization
- `config_deserialize_empty_yaml` — Empty YAML produces defaults
- `config_deserialize_partial_yaml` — Partial YAML fills in defaults
- `config_deserialize_security_patterns` — Blocked patterns parsed
- `config_deserialize_unicode_admin_wxid` — Unicode wxid accepted

### Path Expansion (`expanded_data_dir`)
- `expanded_data_dir_tilde_prefix` — `~/foo` expands to home
- `expanded_data_dir_tilde_slash` — `~/` expands correctly
- `expanded_data_dir_tilde_only` — `~` alone expands
- `expanded_data_dir_absolute_path` — `/absolute/path` unchanged
- `expanded_data_dir_relative_path` — `relative/path` unchanged

## database.rs — Database Tests (`database::tests::`)

### Friends
- `friend_upsert_and_get` — Basic insert and retrieve
- `friend_upsert_overwrites_with_explicit_values` — Upsert overwrites
- `friend_upsert_preserves_fields_on_conflict` — Upsert preserves unset fields
- `friend_permission` — Permission get/set
- `friend_list_and_remove` — Listing and deletion
- `friend_find_by_nickname` — Search by nickname
- `friend_find_by_nickname_matches_remark_name` — Search matches remark name
- `friend_find_by_nickname_empty_search` — Empty search string
- `friend_find_by_nickname_sql_wildcard` — SQL wildcard characters in search
- `friend_unicode_wxid` — Unicode wxid storage
- `friend_emoji_nickname` — Emoji in nicknames
- `friend_very_long_nickname` — Very long nickname strings
- `friend_empty_wxid` — Empty wxid handling
- `friend_special_chars_in_notes` — Special characters in notes
- `friend_invalid_permission_rejected` — Invalid permission string

### Sessions
- `session_lifecycle` — Create, get, touch, expire
- `session_multiple_sessions_returns_latest` — Latest session returned
- `session_touch_increments_count_multiple_times` — Touch counter
- `session_get_active_nonexistent_user` — Nonexistent user returns None
- `session_clean_expired_zero_minutes` — Zero-minute cleanup
- `session_clean_expired_large_window_keeps_sessions` — Large window keeps all

### Rate Limits
- `rate_limiting` — Basic rate limit enforcement
- `rate_limit_zero_limits` — Zero limits block everything
- `rate_limit_daily_boundary` — Daily limit boundary
- `rate_limit_independent_users` — Per-user independence
- `rate_limit_per_minute_reason_message` — Correct error message
- `rate_limit_cleanup_runs` — Cleanup of old entries

### Audit Log
- `audit_log_and_query` — Basic log and query
- `audit_get_recent_with_limit` — Query with limit
- `audit_log_with_null_message` — Null/empty messages
- `audit_log_with_very_long_message` — Very long messages
- `audit_direction_constraint` — Direction field validation

## docker_manager.rs — Docker Tests (`docker_manager::tests::`)

### Container Naming
- `test_container_name_sanitization` — Basic sanitization
- `test_container_name_special_chars` — Special characters
- `test_container_name_chinese_characters` — Chinese character wxids
- `test_container_name_emoji` — Emoji in wxids
- `test_container_name_very_long_wxid` — Long wxid truncation
- `test_container_name_empty_wxid` — Empty wxid
- `test_container_name_all_special_chars` — All special chars
- `test_container_name_dots_and_hyphens_preserved` — Dots/hyphens kept

### Permissions & Config
- `test_permission_display` — Permission Display trait
- `test_permission_display_trait` — Display formatting
- `test_permission_equality` — Equality comparison
- `test_permission_copy` — Copy semantics
- `test_default_config` — Default DockerConfig values
- `test_default_limits_reasonable` — Limits are reasonable
- `test_calculate_cpu_percent_no_delta` — CPU percent with no delta

## message_router.rs — Router Tests (`message_router::tests::`)

### Display Name Resolution
- `display_name_prefers_remark_name` — Remark name first priority
- `display_name_falls_back_to_nickname` — Nickname as fallback
- `display_name_falls_back_to_wxid` — wxid as last resort
- `display_name_unicode_nickname` — Unicode nicknames
- `display_name_emoji_remark` — Emoji remark names

### Log Formatting
- `format_logs_single_incoming` — Single incoming log entry
- `format_logs_outgoing` — Outgoing log entry
- `format_logs_empty` — Empty log list
- `format_logs_no_nickname` — Missing nickname
- `format_logs_no_timestamp` — Missing timestamp
- `format_logs_long_message_truncated` — Truncated long messages
- `format_logs_unicode_message` — Unicode in log messages

### Byte Formatting
- `format_bytes_zero` — 0 bytes
- `format_bytes_bytes_range` — Bytes range (< 1KB)
- `format_bytes_kilobytes` — Kilobytes
- `format_bytes_megabytes` — Megabytes
- `format_bytes_gigabytes` — Gigabytes
- `format_bytes_exact_boundary` — Exact 1024 boundaries

### Permission Levels
- `perm_level_admin_is_highest` — Admin highest level
- `perm_level_trusted_is_middle` — Trusted middle level
- `perm_level_normal_is_low` — Normal low level
- `perm_level_unknown_is_zero` — Unknown defaults to zero

## claude_executor.rs — Executor Tests (`claude_executor::tests::`)

### Permission Parsing
- `parse_permission_admin` — "admin" maps to Admin
- `parse_permission_trusted` — "trusted" maps to Trusted
- `parse_permission_normal` — "normal" maps to Normal
- `parse_permission_unknown_defaults_to_normal` — Unknown defaults to Normal

### Session Expiry
- `session_expired_old_timestamp` — Old timestamps are expired
- `session_not_expired_recent` — Recent timestamps not expired
- `session_expired_with_zero_window` — Zero window edge case
- `session_expired_invalid_format` — Invalid format treated as expired
- `session_expired_iso8601_format_not_supported` — T-separator not supported
- `session_expired_max_window` — u64::MAX window
- `session_expired_future_timestamp` — Future timestamps not expired
