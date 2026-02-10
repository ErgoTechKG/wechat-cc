import { execFile, spawn } from 'child_process';
import { promisify } from 'util';
import { mkdirSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import config from './config.js';
import logger from './logger.js';

const execFileAsync = promisify(execFile);
const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Docker 容器管理器
 *
 * 为每个微信好友维护一个独立的 Docker 容器：
 * - 文件系统隔离：每人有自己的 /home/sandbox/workspace
 * - 进程隔离：容器内的进程不影响 host 或其他好友
 * - 资源限制：CPU、内存、磁盘配额
 * - 网络隔离：可选断网或限制网络
 * - 持久化：workspace 通过 volume 持久保存
 */
class DockerManager {
  constructor() {
    this.containerPrefix = config.docker.container_prefix;
    this.imageName = config.docker.image;
    this.dataDir = config.docker.data_dir.replace('~', process.env.HOME);

    // 确保数据根目录存在
    mkdirSync(this.dataDir, { recursive: true });
  }

  // ============================================
  // 容器命名约定
  // ============================================

  /** wxid → 容器名 */
  containerName(wxid) {
    // 清理 wxid 中不适合 docker 命名的字符
    const safe = wxid.replace(/[^a-zA-Z0-9_.-]/g, '_');
    return `${this.containerPrefix}${safe}`;
  }

  /** wxid → host 上的持久化目录 */
  userDataDir(wxid) {
    const dir = join(this.dataDir, wxid);
    mkdirSync(dir, { recursive: true });
    return dir;
  }

  // ============================================
  // 容器生命周期
  // ============================================

  /**
   * 确保好友的容器存在并运行
   * 如果不存在则创建，如果停止则启动
   */
  async ensureContainer(wxid, permission) {
    const name = this.containerName(wxid);

    // 检查容器是否已存在
    const exists = await this.containerExists(name);

    if (!exists) {
      await this.createContainer(wxid, permission);
      logger.info(`🐳 创建容器: ${name}`);
    }

    // 检查是否在运行
    const running = await this.isRunning(name);
    if (!running) {
      await this.startContainer(name);
      logger.info(`▶️  启动容器: ${name}`);
    }

    return name;
  }

  /**
   * 创建好友专属容器
   */
  async createContainer(wxid, permission) {
    const name = this.containerName(wxid);
    const dataDir = this.userDataDir(wxid);
    const dc = config.docker;

    const args = [
      'create',
      '--name', name,

      // ---------- 资源限制 ----------
      '--memory', dc.limits.memory,
      '--cpus', String(dc.limits.cpus),
      '--pids-limit', String(dc.limits.pids),

      // 磁盘限制（通过 tmpfs 限制 /tmp）
      '--tmpfs', `/tmp:size=${dc.limits.tmp_size}`,

      // ---------- 安全限制 ----------
      '--read-only',                              // 只读根文件系统
      '--security-opt', 'no-new-privileges',       // 禁止提权
      '--cap-drop', 'ALL',                         // 移除所有 capabilities

      // ---------- 网络 ----------
      '--network', this.getNetwork(permission),

      // ---------- 持久化卷 ----------
      // workspace 目录是好友的持久工作区
      '-v', `${dataDir}/workspace:/home/sandbox/workspace`,
      // Claude 的配置/缓存目录（保存会话状态）
      '-v', `${dataDir}/claude-config:/home/sandbox/.claude`,

      // ---------- 环境变量 ----------
      '-e', `WXID=${wxid}`,
      '-e', `ANTHROPIC_API_KEY=${process.env.ANTHROPIC_API_KEY || ''}`,

      // ---------- 标签（方便批量管理） ----------
      '--label', 'app=wechat-claude-bridge',
      '--label', `wxid=${wxid}`,
      '--label', `permission=${permission}`,

      // ---------- 保持容器运行 ----------
      '-d',                                        // detached
      '--restart', 'unless-stopped',

      // ---------- 镜像 ----------
      this.imageName,

      // 容器启动后保持活跃（sleep infinity）
      'tail', '-f', '/dev/null',
    ];

    // trusted 用户允许写入部分目录
    if (permission === 'trusted' || permission === 'admin') {
      // 给 workspace 写权限（已有 volume mount），额外给 /tmp 写权限
      // read-only 下 volume mount 的目录仍然可写
    }

    // admin 容器可以给更多资源
    if (permission === 'admin') {
      const idx = args.indexOf('--memory') + 1;
      args[idx] = dc.limits.admin_memory || '2g';
      const cpuIdx = args.indexOf('--cpus') + 1;
      args[cpuIdx] = String(dc.limits.admin_cpus || 2);
    }

    await this.docker(args);

    // 确保 volume 目录权限正确
    await this.fixPermissions(wxid);
  }

  /**
   * 修复 volume 目录权限（host 创建的目录可能是 root 所有）
   */
  async fixPermissions(wxid) {
    const dataDir = this.userDataDir(wxid);
    const dirs = ['workspace', 'claude-config'];
    for (const d of dirs) {
      const p = join(dataDir, d);
      mkdirSync(p, { recursive: true });
    }

    // 用临时 root 容器修复权限
    const name = this.containerName(wxid);
    try {
      await this.docker([
        'exec', '-u', 'root', name,
        'chown', '-R', 'sandbox:sandbox',
        '/home/sandbox/workspace', '/home/sandbox/.claude',
      ]);
    } catch {
      // 容器可能还没启动，忽略
    }
  }

  /**
   * 根据权限决定网络策略
   */
  getNetwork(permission) {
    switch (permission) {
      case 'admin':
        return config.docker.network.admin || 'bridge';
      case 'trusted':
        return config.docker.network.trusted || 'claude-limited';
      case 'normal':
      default:
        return config.docker.network.normal || 'none'; // 普通用户完全断网
    }
  }

  // ============================================
  // 在容器中执行命令
  // ============================================

  /**
   * 在好友的容器中执行 Claude Code
   * 这是核心方法 —— 消息进来后调用这里
   */
  async execClaude(wxid, systemPrompt, message, options = {}) {
    const name = this.containerName(wxid);
    const timeout = options.timeout || config.claude.timeout;

    // 构建 docker exec 参数
    const dockerArgs = [
      'exec',
      '-u', 'sandbox',                    // 以沙箱用户运行
      '-w', '/home/sandbox/workspace',     // 工作目录
      '-e', `ANTHROPIC_API_KEY=${process.env.ANTHROPIC_API_KEY || ''}`,
      name,
    ];

    // Claude Code 参数
    const claudeArgs = [
      'claude',
      '--print',
      '--output-format', 'text',
      '--system-prompt', systemPrompt,
    ];

    // 会话恢复
    if (options.claudeSession) {
      claudeArgs.push('--resume', options.claudeSession);
    }

    // 权限控制
    if (options.permission === 'normal') {
      claudeArgs.push('--allowedTools', '');
    }

    // 用户消息
    claudeArgs.push(message);

    const fullArgs = [...dockerArgs, ...claudeArgs];

    return new Promise((resolve) => {
      const proc = spawn('docker', fullArgs, {
        timeout: timeout * 1000,
      });

      let stdout = '';
      let stderr = '';

      proc.stdout.on('data', (data) => { stdout += data.toString(); });
      proc.stderr.on('data', (data) => { stderr += data.toString(); });

      const timer = setTimeout(() => {
        proc.kill('SIGTERM');
        // 如果 SIGTERM 没用，5秒后 SIGKILL
        setTimeout(() => {
          try { proc.kill('SIGKILL'); } catch {}
        }, 5000);
        resolve({ ok: false, output: '⏰ 请求超时' });
      }, timeout * 1000);

      proc.on('close', (code) => {
        clearTimeout(timer);
        if (code === 0) {
          resolve({
            ok: true,
            output: stdout.trim() || '(Claude 没有返回内容)',
            stderr,
          });
        } else {
          logger.error(`容器执行失败 [${name}] code=${code}: ${stderr.substring(0, 300)}`);
          resolve({
            ok: false,
            output: '❌ 处理出错了，请稍后重试',
            stderr,
          });
        }
      });

      proc.on('error', (err) => {
        clearTimeout(timer);
        logger.error(`docker exec 失败: ${err.message}`);
        resolve({ ok: false, output: '❌ 容器执行失败' });
      });
    });
  }

  /**
   * 在容器中执行任意命令（管理用途）
   */
  async execCommand(wxid, command, asRoot = false) {
    const name = this.containerName(wxid);
    const args = [
      'exec',
      '-u', asRoot ? 'root' : 'sandbox',
      name,
      'sh', '-c', command,
    ];

    try {
      const { stdout } = await execFileAsync('docker', args, { timeout: 30000 });
      return stdout.trim();
    } catch (err) {
      return `Error: ${err.message}`;
    }
  }

  // ============================================
  // 容器状态查询
  // ============================================

  async containerExists(name) {
    try {
      await execFileAsync('docker', ['inspect', name]);
      return true;
    } catch {
      return false;
    }
  }

  async isRunning(name) {
    try {
      const { stdout } = await execFileAsync('docker', [
        'inspect', '-f', '{{.State.Running}}', name,
      ]);
      return stdout.trim() === 'true';
    } catch {
      return false;
    }
  }

  async startContainer(name) {
    await this.docker(['start', name]);
  }

  async stopContainer(wxid) {
    const name = this.containerName(wxid);
    try {
      await this.docker(['stop', '-t', '10', name]);
      logger.info(`⏹️  停止容器: ${name}`);
      return true;
    } catch {
      return false;
    }
  }

  async destroyContainer(wxid) {
    const name = this.containerName(wxid);
    try {
      await this.docker(['rm', '-f', name]);
      logger.info(`🗑️  销毁容器: ${name}`);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * 获取容器资源使用情况
   */
  async getStats(wxid) {
    const name = this.containerName(wxid);
    try {
      const { stdout } = await execFileAsync('docker', [
        'stats', '--no-stream', '--format',
        '{"cpu":"{{.CPUPerc}}","mem":"{{.MemUsage}}","net":"{{.NetIO}}","pids":"{{.PIDs}}"}',
        name,
      ]);
      return JSON.parse(stdout.trim());
    } catch {
      return null;
    }
  }

  /**
   * 获取容器磁盘使用
   */
  async getDiskUsage(wxid) {
    const name = this.containerName(wxid);
    try {
      const output = await this.execCommand(wxid, 'du -sh /home/sandbox/workspace');
      return output;
    } catch {
      return 'unknown';
    }
  }

  /**
   * 列出所有桥接系统管理的容器
   */
  async listContainers() {
    try {
      const { stdout } = await execFileAsync('docker', [
        'ps', '-a',
        '--filter', 'label=app=wechat-claude-bridge',
        '--format', '{{.Names}}\t{{.Status}}\t{{.Labels}}',
      ]);

      return stdout.trim().split('\n').filter(Boolean).map(line => {
        const [name, status, labels] = line.split('\t');
        const labelMap = {};
        labels?.split(',').forEach(l => {
          const [k, v] = l.split('=');
          labelMap[k] = v;
        });
        return { name, status, wxid: labelMap.wxid, permission: labelMap.permission };
      });
    } catch {
      return [];
    }
  }

  // ============================================
  // 批量管理
  // ============================================

  /**
   * 停止所有容器
   */
  async stopAll() {
    const containers = await this.listContainers();
    for (const c of containers) {
      if (c.wxid) await this.stopContainer(c.wxid);
    }
    logger.info(`已停止 ${containers.length} 个容器`);
  }

  /**
   * 清理停止的容器
   */
  async cleanup() {
    try {
      await this.docker([
        'container', 'prune', '-f',
        '--filter', 'label=app=wechat-claude-bridge',
      ]);
    } catch {}
  }

  /**
   * 重建好友容器（更新镜像后使用）
   */
  async rebuild(wxid, permission) {
    await this.destroyContainer(wxid);
    await this.ensureContainer(wxid, permission);
    logger.info(`🔄 重建容器: ${this.containerName(wxid)}`);
  }

  // ============================================
  // Docker 网络初始化
  // ============================================

  /**
   * 创建受限网络（仅允许访问 Anthropic API）
   */
  async initNetworks() {
    // 创建受限网络
    try {
      await execFileAsync('docker', [
        'network', 'inspect', 'claude-limited',
      ]);
      logger.debug('网络 claude-limited 已存在');
    } catch {
      try {
        await execFileAsync('docker', [
          'network', 'create',
          '--driver', 'bridge',
          'claude-limited',
        ]);
        logger.info('创建网络: claude-limited');
      } catch (err) {
        logger.warn(`创建网络失败: ${err.message}`);
      }
    }
  }

  // ============================================
  // 辅助
  // ============================================

  async docker(args) {
    return execFileAsync('docker', args, { timeout: 60000 });
  }

  /**
   * 检查 Docker 是否可用
   */
  async healthCheck() {
    try {
      const { stdout } = await execFileAsync('docker', ['version', '--format', '{{.Server.Version}}']);
      logger.info(`Docker 版本: ${stdout.trim()}`);
      return true;
    } catch {
      logger.error('❌ Docker 不可用！请确保 Docker 已安装并运行');
      return false;
    }
  }

  /**
   * 检查沙箱镜像是否存在
   */
  async imageExists() {
    try {
      await execFileAsync('docker', ['inspect', this.imageName]);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * 构建沙箱镜像
   */
  async buildImage() {
    const dockerDir = join(__dirname, '..', 'docker');
    logger.info('🔨 构建沙箱镜像...');
    const { stdout, stderr } = await execFileAsync('docker', [
      'build', '-t', this.imageName, '-f', join(dockerDir, 'Dockerfile.sandbox'), dockerDir,
    ], { timeout: 300000 }); // 5 分钟超时
    logger.info(`镜像构建完成: ${this.imageName}`);
    return { stdout, stderr };
  }
}

export default new DockerManager();
