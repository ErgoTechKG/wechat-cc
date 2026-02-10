import config from './config.js';
import logger from './logger.js';
import { friendsDB, auditDB, rateLimitDB, sessionsDB } from './database.js';
import claudeExecutor from './claude-executor.js';

/**
 * 消息路由器 (Docker 沙箱版)
 *
 * 新增容器管理命令：
 *   /status    — 显示容器状态 + 资源使用
 *   /restart   — 重启好友容器
 *   /destroy   — 销毁容器（保留数据）
 *   /rebuild   — 重建容器（更新镜像后使用）
 *   /containers— 查看所有容器
 */
class MessageRouter {
  constructor() {
    this.commands = new Map();
    this.registerBuiltinCommands();
  }

  // ============================================
  // 核心路由
  // ============================================

  async handleMessage(contact, message) {
    const { wxid, nickname, remarkName } = contact;
    const displayName = remarkName || nickname || wxid;

    logger.info(`📩 收到消息 [${displayName}(${wxid})]: ${message.substring(0, 100)}`);
    auditDB.log(wxid, displayName, 'in', config.logging.log_message_content ? message : '[已隐藏]');

    this.ensureFriendRegistered(wxid, nickname, remarkName);

    const permission = this.getEffectivePermission(wxid);

    if (permission === 'blocked') {
      logger.warn(`🚫 拒绝黑名单用户: ${displayName}(${wxid})`);
      return null;
    }

    if (!permission) {
      return config.permissions.notify_unauthorized
        ? config.permissions.unauthorized_message
        : null;
    }

    // 速率限制
    const rateCheck = rateLimitDB.checkAndIncrement(
      wxid, config.rate_limit.max_per_minute, config.rate_limit.max_per_day
    );
    if (!rateCheck.allowed) {
      return `⚠️ ${rateCheck.reason}`;
    }

    // 内置命令
    if (message.startsWith('/')) {
      const response = await this.handleCommand(wxid, permission, message);
      if (response !== null) {
        auditDB.log(wxid, displayName, 'out', response.substring(0, 200));
        return response;
      }
    }

    // 安全检查
    const secCheck = this.securityCheck(message, permission);
    if (!secCheck.safe) return `⚠️ ${secCheck.reason}`;

    // 转发给 Claude Code (Docker 容器内)
    const friendInfo = friendsDB.get(wxid);
    try {
      const response = await claudeExecutor.execute(wxid, friendInfo, message);
      auditDB.log(wxid, displayName, 'out', response.substring(0, 500));
      logger.info(`📤 回复 [${displayName}]: ${response.substring(0, 100)}...`);
      return response;
    } catch (error) {
      logger.error(`处理消息失败 [${displayName}]: ${error.message}`);
      return '❌ 处理消息时出错了，请稍后重试';
    }
  }

  // ============================================
  // 权限
  // ============================================

  getEffectivePermission(wxid) {
    if (wxid === config.admin_wxid) return 'admin';
    const dbPerm = friendsDB.getPermission(wxid);
    return dbPerm || config.permissions.default_level;
  }

  ensureFriendRegistered(wxid, nickname, remarkName) {
    const existing = friendsDB.get(wxid);
    if (!existing) {
      friendsDB.upsert(wxid, {
        nickname,
        remark_name: remarkName,
        permission: wxid === config.admin_wxid ? 'admin' : config.permissions.default_level,
      });
      logger.info(`新好友注册: ${nickname}(${wxid})`);
    } else if (existing.nickname !== nickname || existing.remark_name !== remarkName) {
      friendsDB.upsert(wxid, { nickname, remark_name: remarkName });
    }
  }

  // ============================================
  // 命令系统
  // ============================================

  registerBuiltinCommands() {
    // --- 所有人 ---
    this.commands.set('/help', {
      permission: 'normal',
      description: '查看帮助',
      handler: (wxid, perm) => this.cmdHelp(perm),
    });

    this.commands.set('/status', {
      permission: 'normal',
      description: '查看状态（含容器信息）',
      handler: (wxid) => this.cmdStatus(wxid),
    });

    this.commands.set('/clear', {
      permission: 'normal',
      description: '清除会话历史',
      handler: (wxid) => this.cmdClear(wxid),
    });

    // --- 管理员 ---
    this.commands.set('/allow', {
      permission: 'admin',
      description: '授权好友: /allow 昵称 [trusted|normal]',
      handler: (wxid, perm, args) => this.cmdAllow(args),
    });

    this.commands.set('/block', {
      permission: 'admin',
      description: '拉黑好友: /block 昵称',
      handler: (wxid, perm, args) => this.cmdBlock(args),
    });

    this.commands.set('/list', {
      permission: 'admin',
      description: '列出所有授权好友',
      handler: () => this.cmdList(),
    });

    this.commands.set('/logs', {
      permission: 'admin',
      description: '查看日志: /logs [昵称]',
      handler: (wxid, perm, args) => this.cmdLogs(args),
    });

    this.commands.set('/kill', {
      permission: 'admin',
      description: '终止好友进程: /kill 昵称',
      handler: (wxid, perm, args) => this.cmdKill(args),
    });

    // --- 容器管理（管理员） ---
    this.commands.set('/containers', {
      permission: 'admin',
      description: '查看所有容器状态',
      handler: () => this.cmdContainers(),
    });

    this.commands.set('/restart', {
      permission: 'admin',
      description: '重启容器: /restart 昵称',
      handler: (wxid, perm, args) => this.cmdRestart(args),
    });

    this.commands.set('/destroy', {
      permission: 'admin',
      description: '销毁容器（保留数据）: /destroy 昵称',
      handler: (wxid, perm, args) => this.cmdDestroy(args),
    });

    this.commands.set('/rebuild', {
      permission: 'admin',
      description: '重建容器: /rebuild 昵称',
      handler: (wxid, perm, args) => this.cmdRebuild(args),
    });

    this.commands.set('/stopall', {
      permission: 'admin',
      description: '停止所有容器',
      handler: () => this.cmdStopAll(),
    });
  }

  async handleCommand(wxid, permission, message) {
    const parts = message.trim().split(/\s+/);
    const cmd = parts[0].toLowerCase();
    const args = parts.slice(1).join(' ');

    const command = this.commands.get(cmd);
    if (!command) return null;

    const permLevels = { admin: 3, trusted: 2, normal: 1, blocked: 0 };
    if (permLevels[permission] < permLevels[command.permission]) {
      return '⚠️ 权限不足';
    }

    return await command.handler(wxid, permission, args);
  }

  // ============================================
  // 命令实现 — 基础
  // ============================================

  cmdHelp(permission) {
    const lines = ['📖 可用命令:\n'];
    const permLevels = { admin: 3, trusted: 2, normal: 1, blocked: 0 };

    for (const [name, cmd] of this.commands) {
      if (permLevels[permission] >= permLevels[cmd.permission]) {
        lines.push(`${name} - ${cmd.description}`);
      }
    }
    lines.push('\n直接发送文字消息即可与 Claude 对话');
    return lines.join('\n');
  }

  async cmdStatus(wxid) {
    const friend = friendsDB.get(wxid);
    const session = sessionsDB.getActive(wxid);
    const container = await claudeExecutor.getContainerStatus(wxid);

    const lines = [
      '📊 当前状态:\n',
      `👤 ${friend?.remark_name || friend?.nickname || '未知'}`,
      `🔑 权限: ${friend?.permission || '无'}`,
      `💬 会话: ${session ? `活跃 (${session.message_count} 条消息)` : '无'}`,
      '',
      `🐳 容器: ${container.name}`,
      `   状态: ${container.running ? '✅ 运行中' : '⏹️ 已停止'}`,
    ];

    if (container.stats) {
      lines.push(
        `   CPU: ${container.stats.cpu}`,
        `   内存: ${container.stats.mem}`,
        `   进程: ${container.stats.pids}`,
      );
    }
    if (container.disk) {
      lines.push(`   磁盘: ${container.disk}`);
    }

    return lines.join('\n');
  }

  async cmdClear(wxid) {
    await claudeExecutor.clearSession(wxid, false);
    return '✅ 会话已清除，下次对话将开始新的上下文';
  }

  // ============================================
  // 命令实现 — 好友管理
  // ============================================

  cmdAllow(args) {
    if (!args) return '用法: /allow 昵称 [trusted|normal]';

    const parts = args.split(/\s+/);
    const searchName = parts[0];
    const level = parts[1] || 'trusted';

    if (!['trusted', 'normal', 'admin'].includes(level)) {
      return '❌ 无效权限等级，可选: trusted, normal, admin';
    }

    const matches = friendsDB.findByNickname(searchName);
    if (matches.length === 0) {
      return `❌ 未找到 "${searchName}"，该好友需要先发一条消息`;
    }
    if (matches.length > 1) {
      const names = matches.map(f => `${f.nickname}(${f.wxid})`).join('\n');
      return `找到多个匹配:\n${names}\n请精确指定`;
    }

    const friend = matches[0];
    friendsDB.setPermission(friend.wxid, level);
    logger.info(`权限变更: ${friend.nickname} -> ${level}`);
    return `✅ ${friend.nickname} → ${level}`;
  }

  async cmdBlock(args) {
    if (!args) return '用法: /block 昵称';

    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;
    if (matches.length > 1) return '找到多个匹配，请精确指定';

    const friend = matches[0];
    friendsDB.setPermission(friend.wxid, 'blocked');
    await claudeExecutor.destroyContainer(friend.wxid);
    logger.info(`已拉黑并销毁容器: ${friend.nickname}`);
    return `🚫 已拉黑 ${friend.nickname}，容器已销毁`;
  }

  cmdList() {
    const friends = friendsDB.listAll();
    if (friends.length === 0) return '暂无授权好友';

    const lines = ['👥 好友列表:\n'];
    const grouped = {};
    friends.forEach(f => {
      if (!grouped[f.permission]) grouped[f.permission] = [];
      grouped[f.permission].push(f);
    });

    const order = ['admin', 'trusted', 'normal', 'blocked'];
    const icons = { admin: '👑', trusted: '⭐', normal: '👤', blocked: '🚫' };

    for (const perm of order) {
      if (grouped[perm]?.length) {
        lines.push(`${icons[perm]} ${perm.toUpperCase()}:`);
        grouped[perm].forEach(f => {
          lines.push(`  ${f.remark_name || f.nickname || f.wxid}`);
        });
        lines.push('');
      }
    }
    return lines.join('\n');
  }

  cmdLogs(args) {
    if (!args) {
      return this.formatLogs(auditDB.getRecent(20));
    }
    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;
    return this.formatLogs(auditDB.getByUser(matches[0].wxid, 20));
  }

  async cmdKill(args) {
    if (!args) return '用法: /kill 昵称';
    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;

    const killed = await claudeExecutor.killProcess(matches[0].wxid);
    return killed ? `✅ 已终止 ${matches[0].nickname} 的进程` : '没有运行中的进程';
  }

  // ============================================
  // 命令实现 — 容器管理
  // ============================================

  async cmdContainers() {
    const containers = await claudeExecutor.listContainers();
    if (containers.length === 0) return '🐳 暂无运行中的容器';

    const lines = ['🐳 容器列表:\n'];
    for (const c of containers) {
      const friend = c.wxid ? friendsDB.get(c.wxid) : null;
      const name = friend?.remark_name || friend?.nickname || c.wxid || '未知';
      const statusIcon = c.status?.includes('Up') ? '✅' : '⏹️';
      lines.push(`${statusIcon} ${name} [${c.permission}]`);
      lines.push(`   ${c.name}: ${c.status}`);
    }
    return lines.join('\n');
  }

  async cmdRestart(args) {
    if (!args) return '用法: /restart 昵称';
    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;

    const friend = matches[0];
    await claudeExecutor.stopContainer(friend.wxid);
    await claudeExecutor.clearSession(friend.wxid, false);
    return `🔄 已重启 ${friend.nickname} 的容器（下次发消息自动启动）`;
  }

  async cmdDestroy(args) {
    if (!args) return '用法: /destroy 昵称';
    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;

    const friend = matches[0];
    await claudeExecutor.destroyContainer(friend.wxid);
    return `🗑️ 已销毁 ${friend.nickname} 的容器（数据保留，下次发消息自动重建）`;
  }

  async cmdRebuild(args) {
    if (!args) return '用法: /rebuild 昵称';
    const matches = friendsDB.findByNickname(args.trim());
    if (matches.length === 0) return `❌ 未找到 "${args}"`;

    const friend = matches[0];
    await claudeExecutor.rebuildContainer(friend.wxid, friend.permission);
    return `🔨 已重建 ${friend.nickname} 的容器`;
  }

  async cmdStopAll() {
    const containers = await claudeExecutor.listContainers();
    for (const c of containers) {
      if (c.wxid) await claudeExecutor.stopContainer(c.wxid);
    }
    return `⏹️ 已停止全部 ${containers.length} 个容器`;
  }

  // ============================================
  // 安全检查
  // ============================================

  securityCheck(message, permission) {
    if (permission === 'admin') return { safe: true };

    for (const pattern of config.security.blocked_patterns) {
      if (new RegExp(pattern, 'i').test(message)) {
        logger.warn(`🚨 安全拦截: ${message.substring(0, 100)}`);
        return { safe: false, reason: '消息包含不允许的操作' };
      }
    }
    return { safe: true };
  }

  // ============================================
  // 辅助
  // ============================================

  formatLogs(logs) {
    if (!logs.length) return '暂无日志';
    return logs.map(l => {
      const dir = l.direction === 'in' ? '📩' : '📤';
      const time = l.timestamp.split(' ')[1] || l.timestamp;
      const msg = l.message?.substring(0, 60) || '';
      return `${dir} [${time}] ${l.nickname}: ${msg}`;
    }).join('\n');
  }
}

export default new MessageRouter();
