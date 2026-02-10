use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

// ============================================
// Data structs
// ============================================

#[derive(Debug, Clone)]
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
pub struct Session {
    pub id: String,
    pub wxid: String,
    pub claude_session: Option<String>,
    pub created_at: Option<String>,
    pub last_active: Option<String>,
    pub message_count: i64,
}

#[derive(Debug, Clone)]
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
        let perm = permission.unwrap_or("normal");
        conn.execute(
            "INSERT INTO friends (wxid, nickname, remark_name, permission, added_by, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(wxid) DO UPDATE SET
               nickname    = COALESCE(excluded.nickname, nickname),
               remark_name = COALESCE(excluded.remark_name, remark_name),
               permission  = COALESCE(excluded.permission, permission),
               notes       = COALESCE(excluded.notes, notes)",
            params![wxid, nickname, remark_name, perm, added_by, notes],
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
            "SELECT id, wxid, claude_session, created_at, last_active, message_count FROM sessions WHERE wxid = ? ORDER BY last_active DESC LIMIT 1",
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
            "DELETE FROM sessions WHERE last_active < datetime('now', '-' || ? || ' minutes')",
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
}
