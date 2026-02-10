import Database from 'better-sqlite3';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { mkdirSync } from 'fs';
import logger from './logger.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DB_PATH = join(__dirname, '..', 'data', 'bridge.db');

// 确保 data 目录存在
mkdirSync(dirname(DB_PATH), { recursive: true });

const db = new Database(DB_PATH);
db.pragma('journal_mode = WAL');
db.pragma('foreign_keys = ON');

// ============================================
// 初始化表结构
// ============================================
db.exec(`
  -- 好友授权表
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

  -- 会话表（每个好友一个活跃会话）
  CREATE TABLE IF NOT EXISTS sessions (
    id             TEXT PRIMARY KEY,
    wxid           TEXT NOT NULL,
    claude_session TEXT,
    created_at     DATETIME DEFAULT CURRENT_TIMESTAMP,
    last_active    DATETIME DEFAULT CURRENT_TIMESTAMP,
    message_count  INTEGER DEFAULT 0,
    FOREIGN KEY (wxid) REFERENCES friends(wxid)
  );

  -- 消息审计日志
  CREATE TABLE IF NOT EXISTS audit_log (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    wxid           TEXT NOT NULL,
    nickname       TEXT,
    direction      TEXT NOT NULL CHECK(direction IN ('in','out')),
    message        TEXT,
    claude_session TEXT,
    timestamp      DATETIME DEFAULT CURRENT_TIMESTAMP
  );

  -- 速率限制跟踪
  CREATE TABLE IF NOT EXISTS rate_limits (
    wxid           TEXT NOT NULL,
    window_start   DATETIME NOT NULL,
    request_count  INTEGER DEFAULT 1,
    PRIMARY KEY (wxid, window_start)
  );

  -- 索引
  CREATE INDEX IF NOT EXISTS idx_audit_wxid ON audit_log(wxid);
  CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(timestamp);
  CREATE INDEX IF NOT EXISTS idx_sessions_wxid ON sessions(wxid);
  CREATE INDEX IF NOT EXISTS idx_rate_wxid ON rate_limits(wxid);
`);

logger.info('数据库初始化完成');

// ============================================
// 好友管理
// ============================================
export const friendsDB = {
  /** 获取好友信息 */
  get(wxid) {
    return db.prepare('SELECT * FROM friends WHERE wxid = ?').get(wxid);
  },

  /** 添加/更新好友 */
  upsert(wxid, { nickname, remark_name, permission, added_by, notes } = {}) {
    const stmt = db.prepare(`
      INSERT INTO friends (wxid, nickname, remark_name, permission, added_by, notes)
      VALUES (?, ?, ?, ?, ?, ?)
      ON CONFLICT(wxid) DO UPDATE SET
        nickname = COALESCE(excluded.nickname, nickname),
        remark_name = COALESCE(excluded.remark_name, remark_name),
        permission = COALESCE(excluded.permission, permission),
        notes = COALESCE(excluded.notes, notes)
    `);
    return stmt.run(wxid, nickname, remark_name, permission || 'normal', added_by, notes);
  },

  /** 获取好友权限等级 */
  getPermission(wxid) {
    const row = db.prepare('SELECT permission FROM friends WHERE wxid = ?').get(wxid);
    return row?.permission || null;
  },

  /** 设置权限 */
  setPermission(wxid, permission) {
    return db.prepare('UPDATE friends SET permission = ? WHERE wxid = ?').run(permission, wxid);
  },

  /** 列出所有授权好友 */
  listAll() {
    return db.prepare('SELECT * FROM friends ORDER BY added_at DESC').all();
  },

  /** 按权限列出 */
  listByPermission(permission) {
    return db.prepare('SELECT * FROM friends WHERE permission = ?').all(permission);
  },

  /** 删除好友 */
  remove(wxid) {
    return db.prepare('DELETE FROM friends WHERE wxid = ?').run(wxid);
  },

  /** 通过昵称查找 */
  findByNickname(nickname) {
    return db.prepare(
      'SELECT * FROM friends WHERE nickname LIKE ? OR remark_name LIKE ?'
    ).all(`%${nickname}%`, `%${nickname}%`);
  },
};

// ============================================
// 会话管理
// ============================================
export const sessionsDB = {
  /** 获取用户活跃会话 */
  getActive(wxid) {
    return db.prepare(
      'SELECT * FROM sessions WHERE wxid = ? ORDER BY last_active DESC LIMIT 1'
    ).get(wxid);
  },

  /** 创建新会话 */
  create(id, wxid, claudeSession = null) {
    return db.prepare(
      'INSERT INTO sessions (id, wxid, claude_session) VALUES (?, ?, ?)'
    ).run(id, wxid, claudeSession);
  },

  /** 更新会话活跃时间 */
  touch(id) {
    return db.prepare(
      'UPDATE sessions SET last_active = CURRENT_TIMESTAMP, message_count = message_count + 1 WHERE id = ?'
    ).run(id);
  },

  /** 更新 Claude 会话ID */
  setClaudeSession(id, claudeSession) {
    return db.prepare(
      'UPDATE sessions SET claude_session = ? WHERE id = ?'
    ).run(claudeSession, id);
  },

  /** 删除用户所有会话 */
  clearUser(wxid) {
    return db.prepare('DELETE FROM sessions WHERE wxid = ?').run(wxid);
  },

  /** 清理过期会话 */
  cleanExpired(expireMinutes) {
    return db.prepare(
      `DELETE FROM sessions WHERE last_active < datetime('now', '-' || ? || ' minutes')`
    ).run(expireMinutes);
  },
};

// ============================================
// 审计日志
// ============================================
export const auditDB = {
  /** 记录消息 */
  log(wxid, nickname, direction, message, claudeSession = null) {
    return db.prepare(
      'INSERT INTO audit_log (wxid, nickname, direction, message, claude_session) VALUES (?, ?, ?, ?, ?)'
    ).run(wxid, nickname, direction, message, claudeSession);
  },

  /** 查询某用户的日志 */
  getByUser(wxid, limit = 50) {
    return db.prepare(
      'SELECT * FROM audit_log WHERE wxid = ? ORDER BY timestamp DESC LIMIT ?'
    ).all(wxid, limit);
  },

  /** 查询最近日志 */
  getRecent(limit = 100) {
    return db.prepare(
      'SELECT * FROM audit_log ORDER BY timestamp DESC LIMIT ?'
    ).all(limit);
  },
};

// ============================================
// 速率限制
// ============================================
export const rateLimitDB = {
  /** 检查并增加计数，返回是否允许 */
  checkAndIncrement(wxid, maxPerMinute, maxPerDay) {
    const now = new Date();

    // 分钟级检查
    const minuteWindow = new Date(now);
    minuteWindow.setSeconds(0, 0);
    const minuteKey = minuteWindow.toISOString();

    const minuteRow = db.prepare(
      'SELECT request_count FROM rate_limits WHERE wxid = ? AND window_start = ?'
    ).get(wxid, minuteKey);

    if (minuteRow && minuteRow.request_count >= maxPerMinute) {
      return { allowed: false, reason: '请求过于频繁，请稍后再试' };
    }

    // 天级检查
    const dayWindow = now.toISOString().split('T')[0];
    const dayCount = db.prepare(
      `SELECT COALESCE(SUM(request_count), 0) as total
       FROM rate_limits WHERE wxid = ? AND window_start >= ?`
    ).get(wxid, dayWindow);

    if (dayCount && dayCount.total >= maxPerDay) {
      return { allowed: false, reason: '今日请求次数已用完' };
    }

    // 增加计数
    db.prepare(`
      INSERT INTO rate_limits (wxid, window_start, request_count)
      VALUES (?, ?, 1)
      ON CONFLICT(wxid, window_start) DO UPDATE SET
        request_count = request_count + 1
    `).run(wxid, minuteKey);

    return { allowed: true };
  },

  /** 清理旧的速率限制记录 */
  cleanup() {
    return db.prepare(
      `DELETE FROM rate_limits WHERE window_start < datetime('now', '-1 day')`
    ).run();
  },
};

export default db;
