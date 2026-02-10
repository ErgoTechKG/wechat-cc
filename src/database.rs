use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

// ============================================
// Data structs
// ============================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Friend {
    pub wxid: String,
    pub nickname: Option<String>,
    pub remark_name: Option<String>,
    pub permission: String,
    pub added_at: Option<String>,
    pub added_by: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Session {
    pub id: String,
    pub wxid: String,
    pub claude_session: Option<String>,
    pub created_at: Option<String>,
    pub last_active: Option<String>,
    pub message_count: i64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuditEntry {
    pub id: i64,
    pub wxid: String,
    pub nickname: Option<String>,
    pub direction: String,
    pub message: Option<String>,
    pub claude_session: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RateLimitResult {
    pub allowed: bool,
    pub reason: Option<String>,
}

// ============================================
// Database wrapper
// ============================================

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open (or create) the database at `path`, defaulting to `data/bridge.db`.
    pub fn new(path: Option<&Path>) -> anyhow::Result<Self> {
        let db_path: PathBuf = match path {
            Some(p) => p.to_path_buf(),
            None => PathBuf::from("data/bridge.db"),
        };

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_tables()?;
        Ok(db)
    }

    fn init_tables(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            -- Friends / authorization table
            CREATE TABLE IF NOT EXISTS friends (
                wxid           TEXT PRIMARY KEY,
                nickname       TEXT,
                remark_name    TEXT,
                permission     TEXT NOT NULL DEFAULT 'normal'
                               CHECK(permission IN ('admin','trusted','normal','blocked')),
                added_at       DATETIME DEFAULT CURRENT_TIMESTAMP,
                added_by       TEXT,
                notes          TEXT
            );

            -- Sessions table (one active session per friend)
            CREATE TABLE IF NOT EXISTS sessions (
                id             TEXT PRIMARY KEY,
                wxid           TEXT NOT NULL,
                claude_session TEXT,
                created_at     DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_active    DATETIME DEFAULT CURRENT_TIMESTAMP,
                message_count  INTEGER DEFAULT 0,
                FOREIGN KEY (wxid) REFERENCES friends(wxid)
            );

            -- Message audit log
            CREATE TABLE IF NOT EXISTS audit_log (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                wxid           TEXT NOT NULL,
                nickname       TEXT,
                direction      TEXT NOT NULL CHECK(direction IN ('in','out')),
                message        TEXT,
                claude_session TEXT,
                timestamp      DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            -- Rate limit tracking
            CREATE TABLE IF NOT EXISTS rate_limits (
                wxid           TEXT NOT NULL,
                window_start   DATETIME NOT NULL,
                request_count  INTEGER DEFAULT 1,
                PRIMARY KEY (wxid, window_start)
            );

            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_audit_wxid ON audit_log(wxid);
            CREATE INDEX IF NOT EXISTS idx_audit_ts   ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_sessions_wxid ON sessions(wxid);
            CREATE INDEX IF NOT EXISTS idx_rate_wxid  ON rate_limits(wxid);
            ",
        )?;
        Ok(())
    }

    // ============================================
    // Friends management
    // ============================================

    pub fn friend_get(&self, wxid: &str) -> anyhow::Result<Option<Friend>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT wxid, nickname, remark_name, permission, added_at, added_by, notes FROM friends WHERE wxid = ?",
        )?;
        let row = stmt
            .query_row(params![wxid], |row| {
                Ok(Friend {
                    wxid: row.get(0)?,
                    nickname: row.get(1)?,
                    remark_name: row.get(2)?,
                    permission: row.get(3)?,
                    added_at: row.get(4)?,
                    added_by: row.get(5)?,
                    notes: row.get(6)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn friend_upsert(
        &self,
        wxid: &str,
        nickname: Option<&str>,
        remark_name: Option<&str>,
        permission: Option<&str>,
        added_by: Option<&str>,
        notes: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // For INSERT, default to "normal" if no permission specified.
        // For UPDATE (ON CONFLICT), pass NULL so COALESCE preserves existing permission.
        let insert_perm = permission.unwrap_or("normal");
        conn.execute(
            "INSERT INTO friends (wxid, nickname, remark_name, permission, added_by, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(wxid) DO UPDATE SET
               nickname    = COALESCE(excluded.nickname, friends.nickname),
               remark_name = COALESCE(excluded.remark_name, friends.remark_name),
               permission  = COALESCE(?7, friends.permission),
               notes       = COALESCE(excluded.notes, friends.notes)",
            params![wxid, nickname, remark_name, insert_perm, added_by, notes, permission],
        )?;
        Ok(())
    }

    pub fn friend_get_permission(&self, wxid: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT permission FROM friends WHERE wxid = ?")?;
        let row = stmt
            .query_row(params![wxid], |row| row.get::<_, String>(0))
            .optional()?;
        Ok(row)
    }

    pub fn friend_set_permission(&self, wxid: &str, permission: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE friends SET permission = ? WHERE wxid = ?",
            params![permission, wxid],
        )?;
        Ok(())
    }

    pub fn friend_list_all(&self) -> anyhow::Result<Vec<Friend>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT wxid, nickname, remark_name, permission, added_at, added_by, notes FROM friends ORDER BY added_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Friend {
                wxid: row.get(0)?,
                nickname: row.get(1)?,
                remark_name: row.get(2)?,
                permission: row.get(3)?,
                added_at: row.get(4)?,
                added_by: row.get(5)?,
                notes: row.get(6)?,
            })
        })?;
        let mut friends = Vec::new();
        for r in rows {
            friends.push(r?);
        }
        Ok(friends)
    }

    #[allow(dead_code)]
    pub fn friend_list_by_permission(&self, permission: &str) -> anyhow::Result<Vec<Friend>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT wxid, nickname, remark_name, permission, added_at, added_by, notes FROM friends WHERE permission = ?",
        )?;
        let rows = stmt.query_map(params![permission], |row| {
            Ok(Friend {
                wxid: row.get(0)?,
                nickname: row.get(1)?,
                remark_name: row.get(2)?,
                permission: row.get(3)?,
                added_at: row.get(4)?,
                added_by: row.get(5)?,
                notes: row.get(6)?,
            })
        })?;
        let mut friends = Vec::new();
        for r in rows {
            friends.push(r?);
        }
        Ok(friends)
    }

    #[allow(dead_code)]
    pub fn friend_remove(&self, wxid: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM friends WHERE wxid = ?", params![wxid])?;
        Ok(())
    }

    pub fn friend_find_by_nickname(&self, nickname: &str) -> anyhow::Result<Vec<Friend>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", nickname);
        let mut stmt = conn.prepare(
            "SELECT wxid, nickname, remark_name, permission, added_at, added_by, notes FROM friends WHERE nickname LIKE ? OR remark_name LIKE ?",
        )?;
        let rows = stmt.query_map(params![pattern, pattern], |row| {
            Ok(Friend {
                wxid: row.get(0)?,
                nickname: row.get(1)?,
                remark_name: row.get(2)?,
                permission: row.get(3)?,
                added_at: row.get(4)?,
                added_by: row.get(5)?,
                notes: row.get(6)?,
            })
        })?;
        let mut friends = Vec::new();
        for r in rows {
            friends.push(r?);
        }
        Ok(friends)
    }

    // ============================================
    // Session management
    // ============================================

    pub fn session_get_active(&self, wxid: &str) -> anyhow::Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, wxid, claude_session, created_at, last_active, message_count FROM sessions WHERE wxid = ? ORDER BY last_active DESC, rowid DESC LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![wxid], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    wxid: row.get(1)?,
                    claude_session: row.get(2)?,
                    created_at: row.get(3)?,
                    last_active: row.get(4)?,
                    message_count: row.get(5)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn session_create(
        &self,
        id: &str,
        wxid: &str,
        claude_session: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, wxid, claude_session) VALUES (?, ?, ?)",
            params![id, wxid, claude_session],
        )?;
        Ok(())
    }

    pub fn session_touch(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET last_active = CURRENT_TIMESTAMP, message_count = message_count + 1 WHERE id = ?",
            params![id],
        )?;
        Ok(())
    }

    pub fn session_set_claude_session(
        &self,
        id: &str,
        claude_session: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET claude_session = ? WHERE id = ?",
            params![claude_session, id],
        )?;
        Ok(())
    }

    pub fn session_clear_user(&self, wxid: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE wxid = ?", params![wxid])?;
        Ok(())
    }

    pub fn session_clean_expired(&self, expire_minutes: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE last_active <= datetime('now', '-' || ? || ' minutes')",
            params![expire_minutes],
        )?;
        Ok(deleted)
    }

    // ============================================
    // Audit log
    // ============================================

    pub fn audit_log(
        &self,
        wxid: &str,
        nickname: Option<&str>,
        direction: &str,
        message: Option<&str>,
        claude_session: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit_log (wxid, nickname, direction, message, claude_session) VALUES (?, ?, ?, ?, ?)",
            params![wxid, nickname, direction, message, claude_session],
        )?;
        Ok(())
    }

    pub fn audit_get_by_user(&self, wxid: &str, limit: i64) -> anyhow::Result<Vec<AuditEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, wxid, nickname, direction, message, claude_session, timestamp FROM audit_log WHERE wxid = ? ORDER BY timestamp DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![wxid, limit], |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                wxid: row.get(1)?,
                nickname: row.get(2)?,
                direction: row.get(3)?,
                message: row.get(4)?,
                claude_session: row.get(5)?,
                timestamp: row.get(6)?,
            })
        })?;
        let mut entries = Vec::new();
        for r in rows {
            entries.push(r?);
        }
        Ok(entries)
    }

    pub fn audit_get_recent(&self, limit: i64) -> anyhow::Result<Vec<AuditEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, wxid, nickname, direction, message, claude_session, timestamp FROM audit_log ORDER BY timestamp DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                wxid: row.get(1)?,
                nickname: row.get(2)?,
                direction: row.get(3)?,
                message: row.get(4)?,
                claude_session: row.get(5)?,
                timestamp: row.get(6)?,
            })
        })?;
        let mut entries = Vec::new();
        for r in rows {
            entries.push(r?);
        }
        Ok(entries)
    }

    // ============================================
    // Rate limiting
    // ============================================

    pub fn rate_limit_check_and_increment(
        &self,
        wxid: &str,
        max_per_minute: i64,
        max_per_day: i64,
    ) -> anyhow::Result<RateLimitResult> {
        let conn = self.conn.lock().unwrap();

        // Build the minute-window key: truncate seconds to 0
        let now = Utc::now();
        let minute_key = now.format("%Y-%m-%dT%H:%M:00").to_string();

        // Per-minute check
        let minute_count: Option<i64> = conn
            .query_row(
                "SELECT request_count FROM rate_limits WHERE wxid = ? AND window_start = ?",
                params![wxid, minute_key],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(count) = minute_count {
            if count >= max_per_minute {
                return Ok(RateLimitResult {
                    allowed: false,
                    reason: Some("Too many requests, please try again later".into()),
                });
            }
        }

        // Per-day check
        let day_key = now.format("%Y-%m-%d").to_string();
        let day_total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(request_count), 0) FROM rate_limits WHERE wxid = ? AND window_start >= ?",
            params![wxid, day_key],
            |row| row.get(0),
        )?;

        if day_total >= max_per_day {
            return Ok(RateLimitResult {
                allowed: false,
                reason: Some("Daily request quota exhausted".into()),
            });
        }

        // Increment counter
        conn.execute(
            "INSERT INTO rate_limits (wxid, window_start, request_count)
             VALUES (?, ?, 1)
             ON CONFLICT(wxid, window_start) DO UPDATE SET
               request_count = request_count + 1",
            params![wxid, minute_key],
        )?;

        Ok(RateLimitResult {
            allowed: true,
            reason: None,
        })
    }

    pub fn rate_limit_cleanup(&self) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM rate_limits WHERE window_start < datetime('now', '-1 day')",
            [],
        )?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::new(Some(Path::new(":memory:"))).expect("failed to create in-memory db")
    }

    #[test]
    fn friend_upsert_and_get() {
        let db = test_db();
        db.friend_upsert("wx_001", Some("Alice"), None, Some("admin"), None, None)
            .unwrap();
        let f = db.friend_get("wx_001").unwrap().unwrap();
        assert_eq!(f.wxid, "wx_001");
        assert_eq!(f.nickname.as_deref(), Some("Alice"));
        assert_eq!(f.permission, "admin");
    }

    #[test]
    fn friend_permission() {
        let db = test_db();
        db.friend_upsert("wx_002", Some("Bob"), None, None, None, None)
            .unwrap();
        assert_eq!(
            db.friend_get_permission("wx_002").unwrap().as_deref(),
            Some("normal")
        );
        db.friend_set_permission("wx_002", "blocked").unwrap();
        assert_eq!(
            db.friend_get_permission("wx_002").unwrap().as_deref(),
            Some("blocked")
        );
    }

    #[test]
    fn friend_list_and_remove() {
        let db = test_db();
        db.friend_upsert("wx_a", Some("A"), None, Some("admin"), None, None)
            .unwrap();
        db.friend_upsert("wx_b", Some("B"), None, Some("normal"), None, None)
            .unwrap();
        assert_eq!(db.friend_list_all().unwrap().len(), 2);
        assert_eq!(db.friend_list_by_permission("admin").unwrap().len(), 1);
        db.friend_remove("wx_a").unwrap();
        assert_eq!(db.friend_list_all().unwrap().len(), 1);
    }

    #[test]
    fn friend_find_by_nickname() {
        let db = test_db();
        db.friend_upsert("wx_c", Some("Charlie"), Some("Chuck"), None, None, None)
            .unwrap();
        assert_eq!(db.friend_find_by_nickname("charl").unwrap().len(), 1);
        assert_eq!(db.friend_find_by_nickname("Chuck").unwrap().len(), 1);
        assert_eq!(db.friend_find_by_nickname("zzz").unwrap().len(), 0);
    }

    #[test]
    fn session_lifecycle() {
        let db = test_db();
        db.friend_upsert("wx_s", Some("Sess"), None, None, None, None)
            .unwrap();
        db.session_create("sess_1", "wx_s", None).unwrap();
        let s = db.session_get_active("wx_s").unwrap().unwrap();
        assert_eq!(s.id, "sess_1");
        assert_eq!(s.message_count, 0);

        db.session_touch("sess_1").unwrap();
        let s = db.session_get_active("wx_s").unwrap().unwrap();
        assert_eq!(s.message_count, 1);

        db.session_set_claude_session("sess_1", "claude_abc")
            .unwrap();
        let s = db.session_get_active("wx_s").unwrap().unwrap();
        assert_eq!(s.claude_session.as_deref(), Some("claude_abc"));

        db.session_clear_user("wx_s").unwrap();
        assert!(db.session_get_active("wx_s").unwrap().is_none());
    }

    #[test]
    fn audit_log_and_query() {
        let db = test_db();
        db.audit_log("wx_a1", Some("Alice"), "in", Some("hello"), None)
            .unwrap();
        db.audit_log("wx_a1", Some("Alice"), "out", Some("hi"), Some("cs_1"))
            .unwrap();
        db.audit_log("wx_b1", Some("Bob"), "in", Some("hey"), None)
            .unwrap();

        let user_logs = db.audit_get_by_user("wx_a1", 50).unwrap();
        assert_eq!(user_logs.len(), 2);

        let recent = db.audit_get_recent(10).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn rate_limiting() {
        let db = test_db();
        // First request should be allowed
        let r = db.rate_limit_check_and_increment("wx_r", 2, 100).unwrap();
        assert!(r.allowed);

        // Second should also be allowed
        let r = db.rate_limit_check_and_increment("wx_r", 2, 100).unwrap();
        assert!(r.allowed);

        // Third should be blocked (per-minute = 2)
        let r = db.rate_limit_check_and_increment("wx_r", 2, 100).unwrap();
        assert!(!r.allowed);
        assert!(r.reason.is_some());
    }

    // ============================================
    // NEW: Rate limit boundary edge cases
    // ============================================

    #[test]
    fn rate_limit_daily_boundary() {
        let db = test_db();
        // Daily limit of 3, per-minute limit of 100 (high so we don't hit it)
        for _ in 0..3 {
            let r = db.rate_limit_check_and_increment("wx_day", 100, 3).unwrap();
            assert!(r.allowed);
        }
        // 4th should be blocked by daily limit
        let r = db.rate_limit_check_and_increment("wx_day", 100, 3).unwrap();
        assert!(!r.allowed);
        assert!(r.reason.as_deref().unwrap().contains("Daily"));
    }

    #[test]
    fn rate_limit_per_minute_reason_message() {
        let db = test_db();
        // Hit the per-minute limit
        let _ = db.rate_limit_check_and_increment("wx_pm", 1, 100).unwrap();
        let r = db.rate_limit_check_and_increment("wx_pm", 1, 100).unwrap();
        assert!(!r.allowed);
        assert!(r.reason.as_deref().unwrap().contains("Too many"));
    }

    #[test]
    fn rate_limit_independent_users() {
        let db = test_db();
        // User A hits limit
        let _ = db.rate_limit_check_and_increment("wx_aa", 1, 100).unwrap();
        let r = db.rate_limit_check_and_increment("wx_aa", 1, 100).unwrap();
        assert!(!r.allowed);

        // User B should still be allowed
        let r = db.rate_limit_check_and_increment("wx_bb", 1, 100).unwrap();
        assert!(r.allowed);
    }

    #[test]
    fn rate_limit_zero_limits() {
        let db = test_db();
        // Zero per-minute: first request blocked immediately
        let r = db.rate_limit_check_and_increment("wx_zero", 0, 100).unwrap();
        // Since minute_count is None (first check), 0 >= 0 is false in the code.
        // Actually: minute_count is None, so the if let Some block is skipped.
        // Then day_total is 0, 0 >= 100 is false. Then it increments.
        // So the first request with max_per_minute=0 is actually ALLOWED.
        assert!(r.allowed);
        // Second request: now minute_count = 1, 1 >= 0 is true -> blocked
        let r = db.rate_limit_check_and_increment("wx_zero", 0, 100).unwrap();
        assert!(!r.allowed);
    }

    #[test]
    fn rate_limit_cleanup_runs() {
        let db = test_db();
        let _ = db.rate_limit_check_and_increment("wx_cl", 10, 100).unwrap();
        // cleanup should not error even on fresh data
        let deleted = db.rate_limit_cleanup().unwrap();
        // Fresh entries are not older than 1 day, so none deleted
        assert_eq!(deleted, 0);
    }

    // ============================================
    // NEW: Unicode/special characters in DB
    // ============================================

    #[test]
    fn friend_unicode_wxid() {
        let db = test_db();
        db.friend_upsert("wxid_中文", Some("中文用户"), None, Some("normal"), None, None)
            .unwrap();
        let f = db.friend_get("wxid_中文").unwrap().unwrap();
        assert_eq!(f.wxid, "wxid_中文");
        assert_eq!(f.nickname.as_deref(), Some("中文用户"));
    }

    #[test]
    fn friend_emoji_nickname() {
        let db = test_db();
        db.friend_upsert("wx_emoji", Some("🎉🎊🎈"), None, None, None, None)
            .unwrap();
        let f = db.friend_get("wx_emoji").unwrap().unwrap();
        assert_eq!(f.nickname.as_deref(), Some("🎉🎊🎈"));
    }

    #[test]
    fn friend_special_chars_in_notes() {
        let db = test_db();
        let notes = "Line1\nLine2\tTab\r\nCRLF\0Null'Quote\"Double";
        db.friend_upsert("wx_special", Some("Test"), None, None, None, Some(notes))
            .unwrap();
        let f = db.friend_get("wx_special").unwrap().unwrap();
        assert_eq!(f.notes.as_deref(), Some(notes));
    }

    #[test]
    fn friend_empty_wxid() {
        let db = test_db();
        // Empty wxid is technically valid in the schema
        db.friend_upsert("", Some("Empty"), None, None, None, None)
            .unwrap();
        let f = db.friend_get("").unwrap().unwrap();
        assert_eq!(f.wxid, "");
    }

    #[test]
    fn friend_very_long_nickname() {
        let db = test_db();
        let long_name = "A".repeat(10000);
        db.friend_upsert("wx_long", Some(&long_name), None, None, None, None)
            .unwrap();
        let f = db.friend_get("wx_long").unwrap().unwrap();
        assert_eq!(f.nickname.as_deref(), Some(long_name.as_str()));
    }

    // ============================================
    // NEW: Friend upsert idempotency / COALESCE behavior
    // ============================================

    #[test]
    fn friend_upsert_preserves_fields_on_conflict() {
        let db = test_db();
        // First insert with all fields
        db.friend_upsert("wx_up", Some("Original"), Some("Remark"), Some("trusted"), Some("admin_wx"), Some("initial notes"))
            .unwrap();

        // Upsert with only wxid and nickname changed
        db.friend_upsert("wx_up", Some("Updated"), None, None, None, None)
            .unwrap();

        let f = db.friend_get("wx_up").unwrap().unwrap();
        assert_eq!(f.nickname.as_deref(), Some("Updated"));
        // COALESCE should preserve remark_name since we passed None (NULL)
        // But actually in the SQL: COALESCE(excluded.remark_name, remark_name)
        // excluded.remark_name is NULL (we passed None), so it keeps original
        assert_eq!(f.remark_name.as_deref(), Some("Remark"));
    }

    #[test]
    fn friend_upsert_overwrites_with_explicit_values() {
        let db = test_db();
        db.friend_upsert("wx_ow", Some("Original"), Some("OldRemark"), Some("normal"), None, None)
            .unwrap();
        db.friend_upsert("wx_ow", Some("New"), Some("NewRemark"), Some("admin"), None, None)
            .unwrap();

        let f = db.friend_get("wx_ow").unwrap().unwrap();
        assert_eq!(f.nickname.as_deref(), Some("New"));
        assert_eq!(f.remark_name.as_deref(), Some("NewRemark"));
        assert_eq!(f.permission, "admin");
    }

    // ============================================
    // NEW: Permission constraint validation
    // ============================================

    #[test]
    fn friend_invalid_permission_rejected() {
        let db = test_db();
        // The CHECK constraint only allows 'admin','trusted','normal','blocked'
        let result = db.friend_upsert("wx_bad", Some("Bad"), None, Some("superuser"), None, None);
        assert!(result.is_err());
    }

    // ============================================
    // NEW: Session edge cases
    // ============================================

    #[test]
    fn session_multiple_sessions_returns_latest() {
        let db = test_db();
        db.friend_upsert("wx_multi", Some("Multi"), None, None, None, None)
            .unwrap();
        db.session_create("sess_old", "wx_multi", None).unwrap();
        // Touch the old session so it has an earlier last_active
        db.session_touch("sess_old").unwrap();

        db.session_create("sess_new", "wx_multi", Some("claude_xyz")).unwrap();

        // session_get_active returns the one with latest last_active
        let s = db.session_get_active("wx_multi").unwrap().unwrap();
        assert_eq!(s.id, "sess_new");
    }

    #[test]
    fn session_get_active_nonexistent_user() {
        let db = test_db();
        let s = db.session_get_active("wx_nonexistent").unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn session_clean_expired_zero_minutes() {
        let db = test_db();
        db.friend_upsert("wx_exp", Some("Exp"), None, None, None, None)
            .unwrap();
        db.session_create("sess_exp", "wx_exp", None).unwrap();

        // With 0 minutes expiry, everything should be expired
        let deleted = db.session_clean_expired(0).unwrap();
        assert_eq!(deleted, 1);

        // Verify it's gone
        let s = db.session_get_active("wx_exp").unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn session_clean_expired_large_window_keeps_sessions() {
        let db = test_db();
        db.friend_upsert("wx_keep", Some("Keep"), None, None, None, None)
            .unwrap();
        db.session_create("sess_keep", "wx_keep", None).unwrap();

        // With a huge window, nothing should expire
        let deleted = db.session_clean_expired(999999).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn session_touch_increments_count_multiple_times() {
        let db = test_db();
        db.friend_upsert("wx_touch", Some("Touch"), None, None, None, None)
            .unwrap();
        db.session_create("sess_t", "wx_touch", None).unwrap();

        for _ in 0..5 {
            db.session_touch("sess_t").unwrap();
        }

        let s = db.session_get_active("wx_touch").unwrap().unwrap();
        assert_eq!(s.message_count, 5);
    }

    // ============================================
    // NEW: Audit log edge cases
    // ============================================

    #[test]
    fn audit_log_with_null_message() {
        let db = test_db();
        db.audit_log("wx_null", Some("Test"), "in", None, None)
            .unwrap();
        let logs = db.audit_get_by_user("wx_null", 10).unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].message.is_none());
    }

    #[test]
    fn audit_log_with_very_long_message() {
        let db = test_db();
        let long_msg = "x".repeat(100_000);
        db.audit_log("wx_long", Some("Test"), "in", Some(&long_msg), None)
            .unwrap();
        let logs = db.audit_get_by_user("wx_long", 10).unwrap();
        assert_eq!(logs[0].message.as_ref().unwrap().len(), 100_000);
    }

    #[test]
    fn audit_get_recent_with_limit() {
        let db = test_db();
        for i in 0..10 {
            db.audit_log("wx_lim", Some("Test"), "in", Some(&format!("msg_{}", i)), None)
                .unwrap();
        }
        let logs = db.audit_get_recent(3).unwrap();
        assert_eq!(logs.len(), 3);
    }

    #[test]
    fn audit_direction_constraint() {
        let db = test_db();
        // Valid directions: 'in' and 'out'
        db.audit_log("wx_dir", None, "in", None, None).unwrap();
        db.audit_log("wx_dir", None, "out", None, None).unwrap();

        // Invalid direction should fail the CHECK constraint
        let result = db.audit_log("wx_dir", None, "invalid", None, None);
        assert!(result.is_err());
    }

    // ============================================
    // NEW: friend_find_by_nickname edge cases
    // ============================================

    #[test]
    fn friend_find_by_nickname_sql_wildcard() {
        let db = test_db();
        db.friend_upsert("wx_wild", Some("100%_complete"), None, None, None, None)
            .unwrap();
        db.friend_upsert("wx_other", Some("nothing special"), None, None, None, None)
            .unwrap();

        // The % in "100%" is treated as a SQL wildcard by the LIKE pattern
        // because it's embedded directly into the pattern. This is a potential
        // issue -- searching for "100%" will match more broadly than expected.
        let matches = db.friend_find_by_nickname("100%").unwrap();
        // Should match at least the one with "100%_complete"
        assert!(!matches.is_empty());
    }

    #[test]
    fn friend_find_by_nickname_empty_search() {
        let db = test_db();
        db.friend_upsert("wx_e1", Some("Alice"), None, None, None, None)
            .unwrap();
        db.friend_upsert("wx_e2", Some("Bob"), None, None, None, None)
            .unwrap();

        // Empty string with LIKE %% should match everything
        let matches = db.friend_find_by_nickname("").unwrap();
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn friend_find_by_nickname_matches_remark_name() {
        let db = test_db();
        db.friend_upsert("wx_rn", Some("RealName"), Some("SearchableRemark"), None, None, None)
            .unwrap();
        let matches = db.friend_find_by_nickname("SearchableRemark").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].wxid, "wx_rn");
    }
}
