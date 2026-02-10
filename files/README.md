# 微信好友 → Claude Code 桥接系统 (Docker 沙箱版)

每个微信好友拥有独立的 Docker 容器环境，彻底隔离进程、文件系统和网络。

## 架构

```
                        ┌─────────────────────────────────────────────┐
                        │              HOST 服务器                      │
                        │                                             │
 微信好友A ──▶ 微信Bot ──▶│  消息路由 ──▶ Docker Manager                  │
 微信好友B ──▶ (wechaty)  │      │          │                           │
 微信好友C ──▶            │      │     ┌────┴──────────────────────┐   │
                        │      │     │  ┌──────────────────────┐  │   │
                        │      │     │  │ 容器A (wxid_aaa)     │  │   │
                        │      │     │  │ Claude Code 进程      │  │   │
                        │      │     │  │ /home/sandbox/workspace│ │   │
                        │      │     │  │ 内存:512M CPU:1核     │  │   │
                        │      │     │  │ 网络:none(断网)       │  │   │
                        │      │     │  └──────────────────────┘  │   │
                        │      │     │  ┌──────────────────────┐  │   │
                        │      │     │  │ 容器B (wxid_bbb)     │  │   │
                        │      │     │  │ Claude Code 进程      │  │   │
                        │      │     │  │ 内存:512M CPU:1核     │  │   │
                        │      │     │  │ 网络:claude-limited   │  │   │
                        │      │     │  └──────────────────────┘  │   │
                        │      │     │  ┌──────────────────────┐  │   │
                        │  SQLite    │  │ 容器C (admin)        │  │   │
                        │  好友/会话  │  │ 内存:2G CPU:2核      │  │   │
                        │  审计日志   │  │ 网络:bridge(完全)    │  │   │
                        │      │     │  └──────────────────────┘  │   │
                        │      │     └────────────────────────────┘   │
                        │      │                                       │
                        │      │     ~/claude-bridge-data/              │
                        │      │       ├── wxid_aaa/                   │
                        │      │       │   ├── workspace/  (持久化)     │
                        │      │       │   └── claude-config/          │
                        │      │       ├── wxid_bbb/                   │
                        │      │       └── wxid_ccc/                   │
                        └─────────────────────────────────────────────┘
```

## 隔离策略

| 维度 | 实现方式 |
|------|----------|
| **进程隔离** | 每人独立容器，PID namespace 隔离 |
| **文件隔离** | 只读根文件系统 + 独立 workspace volume |
| **网络隔离** | normal=断网, trusted=受限网络, admin=完全 |
| **资源限制** | 内存512M, CPU 1核, PID上限100 |
| **权限控制** | 容器内 non-root 用户, drop ALL capabilities |
| **安全加固** | no-new-privileges, 只读 rootfs |

## 快速开始

```bash
# 1. 前提条件
#    - Docker 已安装并运行
#    - Node.js 20+
#    - Claude Code CLI 已安装 (npm i -g @anthropic-ai/claude-code)
#    - ANTHROPIC_API_KEY 环境变量已设置

# 2. 安装
git clone <repo> && cd wechat-claude-bridge
npm install

# 3. 配置
cp config.example.yaml config.yaml
# 编辑 config.yaml，填入 admin_wxid

# 4. 启动（会自动构建 Docker 镜像）
export ANTHROPIC_API_KEY=sk-ant-xxx
npm start

# 5. 扫码登录微信
```

## 好友权限等级

| 等级 | 容器配置 | 网络 | 能力 |
|------|---------|------|------|
| **admin** | 2G内存, 2核CPU | bridge(完全) | 一切操作 + 管理命令 |
| **trusted** | 512M内存, 1核CPU | 受限(仅API) | 执行代码、文件操作 |
| **normal** | 512M内存, 1核CPU | 无网络 | 仅问答 |
| **blocked** | 无容器 | — | 不响应 |

## 命令列表

### 所有人可用
| 命令 | 说明 |
|------|------|
| (直接发文字) | 与 Claude 对话 |
| `/help` | 查看可用命令 |
| `/status` | 查看状态(含容器资源使用) |
| `/clear` | 清除会话历史 |

### 管理员专属
| 命令 | 说明 |
|------|------|
| `/allow 昵称 [等级]` | 授权好友 |
| `/block 昵称` | 拉黑(销毁容器) |
| `/list` | 好友列表 |
| `/logs [昵称]` | 审计日志 |
| `/kill 昵称` | 终止进程 |
| `/containers` | 查看所有容器 |
| `/restart 昵称` | 重启容器 |
| `/destroy 昵称` | 销毁容器(保留数据) |
| `/rebuild 昵称` | 重建容器(更新镜像后) |
| `/stopall` | 停止全部容器 |

## 文件结构

```
wechat-claude-bridge/
├── config.example.yaml        # 配置模板
├── package.json
├── docker/
│   ├── Dockerfile.sandbox     # 沙箱容器镜像
│   └── docker-compose.yaml    # 镜像构建用
└── src/
    ├── index.js               # 入口：微信Bot + 启动检查
    ├── config.js              # 配置加载
    ├── logger.js              # 日志
    ├── database.js            # SQLite：好友/会话/审计
    ├── message-router.js      # 消息路由 + 命令系统
    ├── docker-manager.js      # 容器生命周期管理
    └── claude-executor.js     # Claude Code 执行(通过Docker)
```

## 数据持久化

每个好友的数据保存在 `~/claude-bridge-data/<wxid>/`：
- `workspace/` — 好友的工作区（代码、文件等）
- `claude-config/` — Claude 会话缓存

容器销毁后数据保留，重建容器时自动挂载。
