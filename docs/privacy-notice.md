# Ramaria 隐私说明

> 版本：v1.0  
> 最后更新：2026-06-12

## 概述

Ramaria 是一个**本地优先**的个人 AI 陪伴记忆系统。本说明解释你的数据在本地和线上的存储与传输方式。

---

## 数据存储位置

所有数据默认存储在本地：

```
%APPDATA%\Ramaria\
├── data\assistant.db          # SQLite 数据库（全部记忆、会话、人格画像）
├── logs\ramaria.log           # 运行日志
├── personas\*.toml            # 人格定义文件
└── config.toml                # 非敏感配置
```

- 数据目录可通过 `RAMARIA_DATA_DIR` 环境变量自定义
- 数据不出本地系统，除非你主动选择使用线上 LLM 后端

---

## 本地模型（LM Studio）

当你选择 LM Studio 作为后端时：

| 数据类型 | 存储位置 | 是否外传 |
|----------|----------|----------|
| 对话消息 | 本地 SQLite | ❌ 不出本地 |
| 记忆摘要（L1） | 本地 SQLite | ❌ 不出本地 |
| 事件/性格（L2/L3） | 本地 SQLite | ❌ 不出本地 |
| 对话 API 请求 | 发送到 `localhost` | ❌ 只发往本机 LM Studio |
| API Key | 不需要 | — |

**结论**：LM Studio 模式下，你的所有数据完全在本地，不上传任何内容到互联网。

---

## 线上模型（DeepSeek / OpenAI）

当你选择 DeepSeek 或 OpenAI 作为后端时：

| 数据类型 | 存储位置 | 是否外传 |
|----------|----------|----------|
| 对话消息 | 本地 SQLite + API 服务器 | ✅ 发送当前对话 |
| 记忆上下文（L1/L2/L3） | 本地 SQLite + API 服务器 | ⚠️ 取决设置 |
| 系统 Prompt（含人格画像） | 本地 SQLite + API 服务器 | ✅ 发送 |
| API Key | Windows 凭据管理器 | ✅ 随 API 请求发送 |

### 线上记忆注入控制

你可以控制是否将本地记忆注入到线上 API 调用中：

| 设置 | 发送内容 |
|------|----------|
| **开启**（默认） | 当前对话 + 相关记忆（L1/L2/L3）+ 人格画像 → API 服务器 |
| **关闭** | 仅当前对话 → API 服务器 |

此开关在设置页面的"隐私设置"中配置。

### API Key 安全

- API Key 存储在 **Windows 凭据管理器**（Credential Manager），不写入配置文件或日志
- Key 通过 HTTPS 加密传输到 API 服务器
- 切换 provider 后可随时在设置中更新 API Key

---

## 隐私确认机制

### 首次确认

使用线上 provider 时，在首次配置向导中展示隐私提示，内容包含：
- 将发送的数据类型（对话、记忆上下文）
- 数据接收方（API 服务器地址）
- 是否持久化确认

### 持久化选项

| 选项 | 行为 |
|------|------|
| ✅ "下次不再提醒" | 确认记录持久化到数据库，跨重启有效 |
| ❌ 不勾选 | 每次重启后重新确认 |

### 变更重确认

以下情况需要重新确认隐私：
- 切换 provider 类型（如从 LM Studio 切换到 DeepSeek）
- 修改 Base URL

切换回本地模型（LM Studio）不会撤销已持久化的线上确认记录。

---

## 日志策略

### 默认行为

- 日志文件：`%APPDATA%\Ramaria\logs\ramaria.log`
- **不记录**：完整用户消息、完整记忆内容、完整 Prompt
- **记录的上下文**：tracing span、请求 ID、耗时、错误链

### 用户消息处理

对话中的用户消息在日志中最多截断前 80 字符，并附带 SHA-256 哈希值用于调试：

```
"用户: 今天天气真好，我想出去走走... [sha256:3f8a9b...]"
```

### 完整 Prompt 日志

提供 `log_full_prompt = true` 配置项（默认关闭）。开启时将显示警告：

```
⚠ 完整 Prompt 日志已开启。Prompt 包含你的对话内容和系统提示。
此设置仅用于调试，建议调试完成后关闭。
```

### LLM 调用日志

每次 LLM 调用记录：
- `trace_id`：请求追踪 ID
- `provider`：后端类型
- `duration_ms`：调用耗时
- `status`：成功/失败
- 不记录请求体和响应体内容

---

## 前端安全

Ramaria 桌面应用使用 WebView 渲染界面，采取以下安全措施：

### 内容安全策略（CSP）

```
default-src 'self';
style-src 'self' 'unsafe-inline';
script-src 'self' 'unsafe-inline';
connect-src 'self' ipc: https://api.deepseek.com https://api.openai.com;
img-src 'self' data:;
font-src 'self'
```

**要点**：
- 禁止加载任何远程脚本（`script-src 'self'`）
- 禁止加载远程图片（`img-src 'self' data:`）
- 字体自托管，不依赖外部 CDN（`font-src 'self'`）
- 网络连接仅允许 Tauri IPC 和配置的 API 端点

### Markdown 安全

LLM 回复中的 Markdown 经过白名单过滤：
- 允许：标题、粗体/斜体、代码块、列表、链接
- **禁止**：原始 HTML、`<script>`、事件处理器、`javascript:` / `data:` 协议
- 所有输出在渲染前 sanitize

### Tauri 权限

桌面应用采用最小权限原则：

| 权限 | 用途 |
|------|------|
| `dialog` | 保存文件对话框（导出）、打开文件夹 |
| `notification` | Windows Toast 桌面通知 |
| `store` | 非敏感配置本地存储 |
| `core:window` | 窗口显示/隐藏/关闭 |

**不具备的权限**：网络文件系统访问、剪贴板读取、进程管理、系统信息收集。

---

## 数据管理

### 数据导出

你可以随时导出全部数据：
- JSON 格式：完整结构化数据（会话、消息、记忆、人格画像）
- Markdown 格式：人类可读格式

### 数据删除

| 操作 | 范围 | 方法 |
|------|------|------|
| 删除会话 | 单个会话 + 关联消息 | 对话页 SessionBar → 右键删除；CLI `ramaria session delete <ID>` |
| 删除全部记忆 | 所有 L1/L2/L3 | 删除 `assistant.db` 文件后重启 |

### 非收集声明

Ramaria **不收集、不上传**以下信息：
- 使用统计数据
- 崩溃报告（本地保存，不自动上传）
- 用户行为分析
- 设备信息

崩溃日志保存在本地，用户可手动发送给开发者用于问题诊断。

---

## 离线使用

- 使用 LM Studio 后端时，Ramaria 可完全离线运行
- 无需注册账号、无需联网
- 自托管字体确保界面在离线环境下完整显示

---

## 联系与反馈

如有隐私相关问题，请通过 [GitHub Issues](https://github.com/entergirl/Ramaria-s/issues) 联系。

---

## 参考

- 桌面使用指南：`rust/docs/desktop-user-guide.md`
- 完整架构说明：`rust/docs/dev/rust-rewrite-analysis.md`
- 项目主页：`https://github.com/entergirl/Ramaria-s`
