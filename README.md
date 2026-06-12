# Ramaria（珊瑚菌）v1.0

> 个人 AI 陪伴记忆系统 — Rust 重写版
>
> 大模型懂一切，唯独不懂你。

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Repo](https://img.shields.io/badge/GitHub-entergirl%2FRamaria--s-black)](https://github.com/entergirl/Ramaria-s)

---

## 简介

Ramaria 是一个**本地优先**的个人 AI 陪伴记忆系统。与普通 AI 聊天应用不同，Ramaria 具备：

- 🧠 **分层记忆体系（L0→L3）**：从对话中自动提取摘要、事件、性格画像
- 🎭 **人格画像推断**：自动识别对话对象的性格特征，LLM 以该人格身份对话
- 🔍 **混合 RAG 检索**：向量检索 + BM25 + 知识图谱三通道融合，Persona-Aware 过滤
- 🔒 **数据完全本地化**：所有记忆存储在本地 SQLite，不上传任何服务器
- 🖥️ **桌面应用 + CLI**：Tauri 桌面应用为主，CLI 为辅

### 与一般 AI 聊天应用的差异

| 维度 | 普通聊天应用 | Ramaria |
|------|------------|---------|
| 记忆 | 每次对话从零开始 | 分层记忆，持续积累 |
| 人格 | 固定 Prompt | 从对话中自动推断，动态演化 |
| 数据 | 全部上传到云端 | 本地存储，LLM 调用可控 |
| 检索 | 无或简单关键词 | 三通道融合 RAG + 遗忘曲线 |

---

## 快速开始

### 系统要求

- **Windows 11**（推荐）/ Windows 10
- 8 GB+ 内存
- 需要 LLM 后端（三选一）：
  - [LM Studio](https://lmstudio.ai/)（免费本地模型，推荐入门）
  - [DeepSeek API Key](https://platform.deepseek.com/)
  - [OpenAI API Key](https://platform.openai.com/)

### 安装

1. 下载 `Ramaria_1.0.0_x64.msi` 安装包
2. 双击运行安装程序，按向导完成安装
3. 桌面出现 Ramaria 快捷方式

### 首次启动

1. 双击 Ramaria 图标
2. 在配置向导中选择 LLM 后端：
   - **LM Studio**：启动 LM Studio 并加载模型 → 填写 `http://localhost:1234/v1`
   - **DeepSeek / OpenAI**：输入 API Key（保存到 Windows 凭据管理器）
3. 配置完成后自动进入对话界面

> 📖 详细指南见 [`rust/docs/desktop-user-guide.md`](docs/desktop-user-guide.md)

### CLI 使用

```bash
# 首次配置
ramaria setup

# 单条提问
ramaria ask "介绍一下你自己"

# 交互对话
ramaria chat

# 查看记忆
ramaria memory --layer l1

# 导出数据
ramaria export --format markdown
```

> 📖 CLI 完整文档见 [`rust/docs/cli-user-guide.md`](docs/cli-user-guide.md)

---

## 项目架构

```
rust/
├── crates/
│   ├── ramaria-core/        # 核心类型、trait、错误、状态机
│   ├── ramaria-storage/     # SQLite、23 张表 migration、全部 Repository
│   ├── ramaria-memory/      # 记忆管线 L0→L3、三通道 RAG、性格推断
│   ├── ramaria-llm/         # LM Studio / DeepSeek / OpenAI 后端
│   ├── ramaria-app/         # 应用用例编排（CLI/Desktop 共用）
│   ├── ramaria-cli/         # 命令行入口（clap）
│   └── ramaria-desktop/     # Tauri 2 桌面应用
├── config/personas/         # 人格定义文件（TOML）
├── docs/                    # 用户文档（使用指南、隐私说明）
│   └── dev/                 # 开发文档（架构、决策、设计）
└── plugins/                 # 编译期插件 trait
```

### 记忆管线

```
L0: messages（原始消息，标记发言人）
  ↓ session 结束 → LLM 压缩
L1: memory_l1（单次会话摘要 + 情感显著性）
  ↓ 未吸收 ≥ 5 条 或 超 7 天 → LLM 提取
L2: memory_events（离散事件 + 8 个推断属性）
  ↓ Phase A 统计 + Phase B LLM 推断 + Phase C 增量更新
L3: personality_traits（三层性格画像 + 置信度追踪）
```

### 检索管线

```
用户消息 + persona_uid
  → Persona-Aware 过滤（按 share 分级）
  → 向量检索 + BM25 + 知识图谱 三通道
  → RRF 融合 + Ebbinghaus 衰减
  → System Prompt 注入
```

---

## 开发

### 构建

```bash
cd rust

# 编译
cargo build --release

# 测试
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# 桌面应用开发模式
cd crates/ramaria-desktop
cargo tauri dev

# CLI 开发模式
cargo run -p ramaria-cli -- ask "hello"
```

### 用户文档

| 文档 | 说明 |
|------|------|
| [`docs/desktop-user-guide.md`](docs/desktop-user-guide.md) | 桌面使用指南 |
| [`docs/cli-user-guide.md`](docs/cli-user-guide.md) | CLI 使用指南 |
| [`docs/privacy-notice.md`](docs/privacy-notice.md) | 隐私说明 |

### 开发者文档

| 文档 | 说明 |
|------|------|
| [`docs/dev/rust-rewrite-analysis.md`](docs/dev/rust-rewrite-analysis.md) | 完整架构计划书（v5.0） |
| [`docs/dev/rust重构决策列表.md`](docs/dev/rust重构决策列表.md) | 决策 SSOT |
| [`docs/dev/rust-migration-map.md`](docs/dev/rust-migration-map.md) | Python→Rust 模块迁移映射 |
| [`docs/dev/development-task-checklist.md`](docs/dev/development-task-checklist.md) | 开发任务清单 |
| [`docs/dev/agent-work-guide.md`](docs/dev/agent-work-guide.md) | Agent 工作指导 |
| [`docs/dev/personality/`](docs/dev/personality/) | 人格画像设计文档 |

---

## 隐私

Ramaria 默认将所有数据存储在本地。详见 [`docs/privacy-notice.md`](docs/privacy-notice.md)。

**要点**：
- LM Studio 模式：所有数据不出本地
- 线上 API：对话内容发送至 API 服务器，可关闭记忆注入
- API Key 保存在 OS 凭据管理器，不写入配置文件
- 日志不记录完整对话内容

---

## 许可证

[MIT](LICENSE)

---

## 与 Python 版的关系

本仓库是 Ramaria 的 Rust 重写版本，位于项目根目录 `rust/` 子目录中。

- **Python v0.7.x**（当前仓库根目录）已进入维护模式，不再活跃开发
- **Rust v1.0**（`rust/` 子目录）是正式替代版本
- 两个版本不共享数据库 schema，Python 旧数据不自动迁移

> 📦 Rust 项目使用嵌套 Git 仓库（`rust/.git`），独立历史、独立远程。
