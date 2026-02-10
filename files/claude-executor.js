import { v4 as uuidv4 } from 'uuid';
import config from './config.js';
import logger from './logger.js';
import { sessionsDB } from './database.js';
import dockerManager from './docker-manager.js';

/**
 * Claude Code 执行器 (Docker 版)
 *
 * 每个好友的 Claude Code 运行在独立 Docker 容器中：
 * - 进程/文件系统/网络 完全隔离
 * - 不影响 host，不影响其他好友
 * - 资源受限（CPU/内存/PID）
 * - workspace 通过 volume 持久化
 */
class ClaudeExecutor {
  constructor() {
    this.activeTasks = new Map(); // wxid -> true (正在执行)
  }

  // ============================================
  // 会话管理
  // ============================================

  getOrCreateSession(wxid) {
    let session = sessionsDB.getActive(wxid);

    if (session) {
      const lastActive = new Date(session.last_active);
      const expireMs = config.session.expire_minutes * 60 * 1000;
      if (Date.now() - lastActive.getTime() > expireMs) {
        logger.info(`会话过期，创建新会话: ${wxid}`);
        sessionsDB.clearUser(wxid);
        session = null;
      }
    }

    if (!session) {
      const sessionId = uuidv4();
      sessionsDB.create(sessionId, wxid);
      session = sessionsDB.getActive(wxid);
      logger.info(`创建新会话: ${wxid} -> ${sessionId}`);
    }

    return session;
  }

  // ============================================
  // 系统提示构建
  // ============================================

  buildSystemPrompt(friendInfo) {
    const { wxid, nickname, remark_name, permission } = friendInfo;
    const displayName = remark_name || nickname || wxid;

    const permDescMap = {
      admin:   '管理员，拥有完全权限，可以执行任何代码和系统操作',
      trusted: '信任用户，可以执行代码和文件操作（沙箱环境内）',
      normal:  '普通用户，仅限问答交流，不可执行代码或访问文件系统',
    };

    return [
      `当前用户身份信息:`,
      `- 微信ID: ${wxid}`,
      `- 昵称: ${displayName}`,
      `- 权限等级: ${permission} (${permDescMap[permission] || '未知'})`,
      ``,
      `环境说明:`,
      `- 你运行在此用户的专属 Docker 容器中`,
      `- 工作目录: /home/sandbox/workspace（持久化存储）`,
      `- 容器与其他用户完全隔离`,
      permission === 'normal'
        ? `- ⚠️ 此用户仅限问答，请勿执行任何代码、shell命令或文件操作`
        : `- 此用户可以请求执行代码和文件操作`,
      `- 回复请保持简洁，适合微信阅读`,
    ].join('\n');
  }

  // ============================================
  // 核心执行
  // ============================================

  /**
   * 执行好友的消息
   * 1. 确保好友的 Docker 容器存在并运行
   * 2. 在容器内执行 Claude Code
   * 3. 返回结果
   */
  async execute(wxid, friendInfo, message) {
    // 防止并发
    if (this.activeTasks.has(wxid)) {
      return '⏳ 上一条消息还在处理中，请稍后再试...';
    }

    this.activeTasks.set(wxid, true);

    try {
      // 1. 确保容器就绪
      await dockerManager.ensureContainer(wxid, friendInfo.permission);

      // 2. 获取/创建会话
      const session = this.getOrCreateSession(wxid);
      sessionsDB.touch(session.id);

      // 3. 构建系统提示
      const systemPrompt = this.buildSystemPrompt(friendInfo);

      // 4. 在容器中执行 Claude Code
      logger.debug(`在容器中执行 Claude [${wxid}]: ${message.substring(0, 80)}...`);

      const result = await dockerManager.execClaude(wxid, systemPrompt, message, {
        claudeSession: session.claude_session,
        permission: friendInfo.permission,
        timeout: config.claude.timeout,
      });

      // 5. 尝试提取会话 ID
      if (result.stderr) {
        this.tryExtractSessionId(session.id, result.stderr);
      }

      // 6. 截断过长响应
      let response = result.output;
      const maxLen = 4000;
      if (response.length > maxLen) {
        response = response.substring(0, maxLen) + '\n\n... (响应过长，已截断)';
      }

      return response;
    } catch (error) {
      logger.error(`执行失败 [${wxid}]: ${error.message}`);
      return '❌ 处理消息时出错，请稍后重试';
    } finally {
      this.activeTasks.delete(wxid);
    }
  }

  // ============================================
  // 容器管理代理方法（供 router 调用）
  // ============================================

  /** 清除用户会话并可选重启容器 */
  async clearSession(wxid, restartContainer = false) {
    sessionsDB.clearUser(wxid);
    if (restartContainer) {
      await dockerManager.stopContainer(wxid);
      await dockerManager.ensureContainer(wxid, 'normal');
    }
    logger.info(`已清除会话: ${wxid}${restartContainer ? ' (容器已重启)' : ''}`);
  }

  /** 终止用户容器中正在运行的进程 */
  async killProcess(wxid) {
    try {
      await dockerManager.execCommand(wxid, 'pkill -f claude || true', true);
      this.activeTasks.delete(wxid);
      return true;
    } catch {
      return false;
    }
  }

  /** 获取容器状态 */
  async getContainerStatus(wxid) {
    const name = dockerManager.containerName(wxid);
    const running = await dockerManager.isRunning(name);
    const stats = running ? await dockerManager.getStats(wxid) : null;
    const disk = running ? await dockerManager.getDiskUsage(wxid) : null;
    return { name, running, stats, disk };
  }

  /** 停止容器 */
  async stopContainer(wxid) {
    return dockerManager.stopContainer(wxid);
  }

  /** 销毁容器（保留数据卷） */
  async destroyContainer(wxid) {
    sessionsDB.clearUser(wxid);
    this.activeTasks.delete(wxid);
    return dockerManager.destroyContainer(wxid);
  }

  /** 重建容器 */
  async rebuildContainer(wxid, permission) {
    sessionsDB.clearUser(wxid);
    this.activeTasks.delete(wxid);
    await dockerManager.rebuild(wxid, permission);
  }

  /** 列出所有容器 */
  async listContainers() {
    return dockerManager.listContainers();
  }

  // ============================================
  // 辅助
  // ============================================

  tryExtractSessionId(sessionId, stderr) {
    const match = stderr.match(/session[:\s]+([a-f0-9-]+)/i);
    if (match) {
      sessionsDB.setClaudeSession(sessionId, match[1]);
      logger.debug(`捕获 Claude 会话ID: ${match[1]}`);
    }
  }
}

export default new ClaudeExecutor();
