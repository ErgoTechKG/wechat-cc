import { WechatyBuilder } from 'wechaty';
import config from './config.js';
import logger from './logger.js';
import messageRouter from './message-router.js';
import { rateLimitDB, sessionsDB } from './database.js';
import dockerManager from './docker-manager.js';

// ============================================
// 微信 Bot 启动
// ============================================

const bot = WechatyBuilder.build({
  name: 'claude-bridge',
  // puppet: 'wechaty-puppet-wechat4u',  // 免费方案，按需启用
});

// --- 登录事件 ---
bot.on('login', (user) => {
  logger.info(`✅ 登录成功: ${user.name()} (${user.id})`);
  logger.info(`管理员微信ID配置为: ${config.admin_wxid || '未设置'}`);

  if (!config.admin_wxid) {
    logger.warn('⚠️  admin_wxid 未设置！请将以下ID添加到 config.yaml:');
    logger.warn(`   admin_wxid: "${user.id}"`);
  }
});

// --- 登出事件 ---
bot.on('logout', (user) => {
  logger.warn(`⚠️ 已登出: ${user.name()}`);
});

// --- 扫码事件 ---
bot.on('scan', (qrcode, status) => {
  if (status === 2) {
    // 生成二维码URL（终端扫码）
    const qrcodeUrl = `https://wechaty.js.org/qrcode/${encodeURIComponent(qrcode)}`;
    logger.info(`📱 请扫码登录: ${qrcodeUrl}`);
    logger.info(`   或在终端查看二维码（安装 qrcode-terminal）`);

    // 尝试在终端显示二维码
    try {
      import('qrcode-terminal').then(mod => {
        mod.default.generate(qrcode, { small: true });
      }).catch(() => {});
    } catch {}
  }
});

// --- 消息事件（核心） ---
bot.on('message', async (msg) => {
  try {
    // 忽略非文本消息
    if (msg.type() !== bot.Message.Type.Text) return;

    // 忽略群消息（只处理私聊）
    const room = msg.room();
    if (room) return;

    // 忽略自己发的消息
    if (msg.self()) return;

    // 获取发送者信息
    const contact = msg.talker();
    const wxid = contact.id;
    const nickname = contact.name();

    // 尝试获取备注名
    let remarkName = '';
    try {
      const alias = await contact.alias();
      remarkName = alias || '';
    } catch {}

    const text = msg.text().trim();
    if (!text) return;

    // 路由消息到处理器
    const response = await messageRouter.handleMessage(
      { wxid, nickname, remarkName },
      text
    );

    // 发送回复
    if (response) {
      // 微信消息长度限制，分段发送
      const chunks = splitMessage(response, 2000);
      for (const chunk of chunks) {
        await contact.say(chunk);
        // 多段消息之间间隔一小会儿
        if (chunks.length > 1) {
          await sleep(500);
        }
      }
    }
  } catch (error) {
    logger.error(`消息处理异常: ${error.message}`, { stack: error.stack });
  }
});

// --- 好友请求事件 ---
bot.on('friendship', async (friendship) => {
  try {
    if (friendship.type() === bot.Friendship.Type.Receive) {
      const contact = friendship.contact();
      logger.info(`📬 收到好友请求: ${contact.name()}`);

      // 可以在这里添加自动通过逻辑
      // await friendship.accept();
    }
  } catch (error) {
    logger.error(`处理好友请求失败: ${error.message}`);
  }
});

// --- 错误事件 ---
bot.on('error', (error) => {
  logger.error(`Bot错误: ${error.message}`);
});

// ============================================
// 辅助函数
// ============================================

function splitMessage(text, maxLen) {
  if (text.length <= maxLen) return [text];

  const chunks = [];
  let remaining = text;
  while (remaining.length > 0) {
    if (remaining.length <= maxLen) {
      chunks.push(remaining);
      break;
    }
    // 尝试在换行处分割
    let splitIdx = remaining.lastIndexOf('\n', maxLen);
    if (splitIdx < maxLen * 0.5) {
      splitIdx = maxLen; // 找不到合适的换行就硬切
    }
    chunks.push(remaining.substring(0, splitIdx));
    remaining = remaining.substring(splitIdx).trimStart();
  }
  return chunks;
}

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

// ============================================
// 定时任务
// ============================================

// 每小时清理过期数据
setInterval(() => {
  sessionsDB.cleanExpired(config.session.expire_minutes);
  rateLimitDB.cleanup();
  logger.debug('定时清理完成');
}, 60 * 60 * 1000);

// ============================================
// 启动
// ============================================

logger.info('🚀 启动微信 → Claude Code 桥接服务...');

// Docker 环境检查
(async () => {
  // 1. 检查 Docker 可用性
  const dockerOk = await dockerManager.healthCheck();
  if (!dockerOk) {
    logger.error('请先安装并启动 Docker: https://docs.docker.com/get-docker/');
    process.exit(1);
  }

  // 2. 检查/构建沙箱镜像
  const imageExists = await dockerManager.imageExists();
  if (!imageExists) {
    logger.info('沙箱镜像不存在，开始构建...');
    try {
      await dockerManager.buildImage();
    } catch (err) {
      logger.error(`镜像构建失败: ${err.message}`);
      logger.error('请手动构建: cd docker && docker compose build sandbox-base');
      process.exit(1);
    }
  }

  // 3. 初始化 Docker 网络
  await dockerManager.initNetworks();

  // 4. 启动微信 Bot
  logger.info('Docker 环境就绪，启动微信 Bot...');
  await bot.start();
  logger.info('Bot 已启动，等待扫码登录...');
})().catch((error) => {
  logger.error(`启动失败: ${error.message}`);
  process.exit(1);
});

// 优雅退出
process.on('SIGINT', async () => {
  logger.info('正在关闭...');
  await bot.stop();
  // 注意：容器设为 restart=unless-stopped，不主动停止
  // 如需停止所有容器，使用 /stopall 命令
  process.exit(0);
});

process.on('SIGTERM', async () => {
  logger.info('正在关闭...');
  await bot.stop();
  process.exit(0);
});
