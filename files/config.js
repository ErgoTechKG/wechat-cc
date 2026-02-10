import { readFileSync, existsSync } from 'fs';
import { parse } from 'yaml';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');

function loadConfig() {
  const configPath = join(ROOT, 'config.yaml');
  if (!existsSync(configPath)) {
    console.error('❌ config.yaml 不存在，请先复制 config.example.yaml 并修改');
    process.exit(1);
  }

  const raw = readFileSync(configPath, 'utf-8');
  const config = parse(raw);

  // 设置默认值
  return {
    admin_wxid: config.admin_wxid || '',
    claude: {
      cli_path: config.claude?.cli_path || 'claude',
      timeout: config.claude?.timeout || 120,
    },
    docker: {
      image: config.docker?.image || 'claude-sandbox:latest',
      container_prefix: config.docker?.container_prefix || 'claude-friend-',
      data_dir: config.docker?.data_dir || '~/claude-bridge-data',
      limits: {
        memory: config.docker?.limits?.memory || '512m',
        admin_memory: config.docker?.limits?.admin_memory || '2g',
        cpus: config.docker?.limits?.cpus || 1,
        admin_cpus: config.docker?.limits?.admin_cpus || 2,
        pids: config.docker?.limits?.pids || 100,
        tmp_size: config.docker?.limits?.tmp_size || '100m',
      },
      network: {
        admin: config.docker?.network?.admin || 'bridge',
        trusted: config.docker?.network?.trusted || 'claude-limited',
        normal: config.docker?.network?.normal || 'none',
      },
    },
    permissions: {
      notify_unauthorized: config.permissions?.notify_unauthorized ?? true,
      unauthorized_message: config.permissions?.unauthorized_message || '抱歉，你还没有被授权使用此服务。',
      default_level: config.permissions?.default_level || 'normal',
    },
    session: {
      expire_minutes: config.session?.expire_minutes || 60,
      max_history: config.session?.max_history || 50,
    },
    rate_limit: {
      max_per_minute: config.rate_limit?.max_per_minute || 10,
      max_per_day: config.rate_limit?.max_per_day || 200,
    },
    security: {
      blocked_patterns: config.security?.blocked_patterns || [],
      trusted_file_access: config.security?.trusted_file_access ?? true,
    },
    logging: {
      level: config.logging?.level || 'info',
      file: config.logging?.file || 'logs/bridge.log',
      log_message_content: config.logging?.log_message_content ?? true,
    },
  };
}

export const config = loadConfig();
export default config;
