# Ramaria CLI 使用指南

> 版本：v1.2（v1.1 + M1 CLI 自动化友好改造与命名规范化：--json/--yes/--quiet、exit code 约定、blocks/utt、memory 层级别名、status、import --dry-run）
> 适用平台：Windows / macOS / Linux

## 概述

`ramaria` 是 Ramaria 的命令行入口，支持对话、记忆查询、会话管理、配置修改、人格管理、数据导入、诊断导出和数据导出。

首次使用前需运行首次配置向导，CLI 与桌面应用共享同一数据目录（`%APPDATA%\Ramaria\`）。

---

## 安装

### 从安装包

Windows 安装包将 `ramaria.exe` 安装到系统 PATH。

### 从源码编译

```bash
cd rust
cargo build --release -p ramaria-cli
# 二进制位于 target/release/ramaria.exe
```

---

## 全局选项

所有子命令都支持以下全局选项：

| 选项 | 说明 |
|------|------|
| `--db <PATH>` | 数据库文件路径。默认 `data/ramaria_assistant.db`，可通过 `RAMARIA_DB_PATH` 环境变量覆盖 |
| `--json` | 全局 JSON 信封输出：stdout 仅输出 `{"ok":true,"data":…}` / `{"ok":false,"error":{...}}`，供脚本/agent 解析（D-V15-011） |
| `--yes` | 自动确认所有确认点（隐私/删除/导入等）。非 TTY 且无 `--yes` 时不挂起、直接失败并提示 |
| `--quiet` | 抑制 stderr 提示（info/success/warn），仅保留错误输出 |
| `--skip-validate` | 跳过 LLM 后端连接验证（仅 `setup` 命令生效） |

---

## 子命令

### `ramaria setup` — 首次配置向导

运行交互式三步配置：选择 Provider → 填写 Base URL / API Key → 确认并保存。

```
ramaria setup
ramaria setup --skip-validate    # 跳过 LLM 连接验证
```

**流程**：
1. 选择 LLM Provider：`lm-studio` / `deepseek` / `openai`
2. 输入 Base URL（预填默认值）和 API Key（线上 provider 必填）
3. 自动扫描 `personas/` 目录注册人格文件
4. 可选 LLM 连接验证（检测后端连通性）
5. 完成配置后标记状态为 Ready

**注意**：
- LM Studio 不需 API Key，但需手动启动 LM Studio 并加载模型
- DeepSeek / OpenAI 的 API Key 保存在 OS keychain，不写入配置文件
- 配置完成后，CLI 可立即进行对话

---

### `ramaria ask` — 单条消息

发送单条消息并获取回复。默认流式输出。

```
ramaria ask "今天天气怎么样"
ramaria ask "介绍一下你自己" --persona rama-0001
ramaria ask "你好" --session "abc-123"
ramaria ask "用一行话解释什么是Rust" --no-stream
ramaria ask "今天心情如何" --json
```

| 参数 | 说明 |
|------|------|
| `<MESSAGE>` | 用户消息文本（支持多词，无需引号也可） |
| `--persona <UID>` | 指定对话人格（默认 `rama-0001`）。可用 `ramaria persona show` 查看可用人格 |
| `--session <UUID>` | 复用已有会话 ID。不指定则自动创建新会话 |
| `--no-stream` | 非流式输出：等待完整回复后一次性打印 |
| `--json` | JSON 事件流输出：每行输出一个 StreamEvent JSON，包含 `request_id`/`delta`/`done`/`error` 字段 |

**示例输出**（默认流式）：
```
🤖 助手 > 今天天气不错，挺风和日丽的。你打算出门吗？
```

---

### `ramaria chat` — 交互式对话

启动 REPL 交互模式，连续对话直到输入 `/exit`。

```
ramaria chat
```

**内置命令**：

| 命令 | 说明 |
|------|------|
| `/exit` 或 `/quit` | 退出对话 |
| `/save` | 保存当前会话（自动关闭并触发 L1 摘要），开始新会话 |
| `/clear` | 开始新会话（自动保存旧会话） |
| `/help` | 显示帮助信息 |

**行为**：
- 每次启动自动创建新会话
- 会话结束时自动生成 L1 摘要（触发记忆管线 L0→L1→L2→L3 级联）
- 后台空闲检测关闭 session 后，下次发消息自动创建新 session 并重试
- 支持流式输出，逐字显示回复
- 不使用 ratatui TUI（v1.1 仅简单 REPL）

---

### `ramaria memory` — 记忆查看

按层级查看系统记忆。

```
ramaria memory                    # 查看 L1 摘要（默认）
ramaria memory --layer l2         # 查看 L2 事件
ramaria memory --layer l3         # 查看 L3 性格画像
ramaria memory --persona rama-0001  # 筛选特定人格
ramaria memory --limit 20         # 限制返回条数
```

| 参数 | 说明 |
|------|------|
| `--layer <l1/l2/l3>` | 记忆层级（默认 `l1`） |
| `--persona <UID>` | 按人格筛选 |
| `--limit <N>` | 返回条数限制 |

**输出格式**：
- **L1**：摘要卡片，含 `summary` / `atmosphere` / `valence` / `salience` / `situation_strength`
- **L2**：事件列表，含 `title` / `confidence` / `keywords` / `attitude`
- **L3**：性格标签表，含 `layer` / `trait` / `meaning` / `confidence`

---

### `ramaria session` — 会话管理

管理对话会话。

```
ramaria session list              # 列出所有会话
ramaria session show <ID>         # 查看会话消息历史
ramaria session delete <ID>       # 删除会话及其关联记忆
```

| 子命令 | 说明 |
|--------|------|
| `list` | 显示全部会话：ID、开始时间、结束时间、消息数。活跃会话标注"活跃" |
| `show <ID>` | 按时间顺序展示该会话的全部消息（含 role 标记） |
| `delete <ID>` | 删除会话及其全部消息。**不可逆**，需交互确认 |

---

### `ramaria config` — 配置管理

查看和修改后端配置。

```
ramaria config list               # 列出当前配置
ramaria config get provider       # 查看当前 provider
ramaria config set provider deepseek     # 切换后端
ramaria config set base-url https://api.deepseek.com  # 修改 Base URL
ramaria config set temperature 0.7       # 修改温度参数
ramaria config set max-tokens 2048       # 修改最大输出 token
```

| 子命令 | 说明 |
|--------|------|
| `list` | 显示全部配置项（API Key 遮蔽显示为 `****`） |
| `get <KEY>` | 查看单项配置 |
| `set <KEY> <VALUE>` | 修改配置项。支持：`provider` / `base-url` / `temperature` / `max-tokens` |

**注意**：
- API Key 不可通过 `config set` 修改（需使用 keychain）
- 切换 provider 后，如为线上 provider，需重新确认隐私
- 配置变更立即生效，无需重启

---

### `ramaria persona` — 人格管理

管理对话人格。

```
ramaria persona show              # 显示所有的人格的名称/类型/状态
ramaria persona reload            # 重新扫描 personas/ 目录并同步到数据库
ramaria persona reload --uid rama-0001   # 仅重新加载指定人格
```

| 子命令 | 说明 |
|--------|------|
| `show` | 列出所有人格：UID、名称、类型（user/rama/char 等）、是否激活、来源 |
| `reload` | 扫描 `personas/` 目录下所有 `.toml` 文件，同步名称/配置到数据库。新人格自动创建，已存在的跳过（幂等操作） |

**人格文件**：
- 源码目录：`config/personas/*.toml`
- 运行期目录：`%APPDATA%\Ramaria\personas\*.toml`
- 文件名即为 persona UID（如 `rama-0001.toml` → UID `rama-0001`）
- 用户可直接用文本编辑器编辑 `.toml` 文件，然后运行 `reload` 同步

---

### `ramaria index` — 索引管理

管理 BM25 和向量索引。

```
ramaria index rebuild             # 重建全部索引
```

- 读取所有 L1/L2 记忆 → 重建 BM25 倒排索引 + 图谱数据
- 显示重建文档数、耗时
- 索引版本不一致时应用会自动进入 Indexing 状态，也可手动触发

---

### `ramaria import` — 聊天记录导入

导入外部聊天记录到 Ramaria。

```
# 快速导入 QQ 聊天记录
ramaria import qq --file chat.txt

# 深度导入（含 L2 事件提取和 L3 性格推断）
ramaria import qq --file chat.txt --deep

# 为导入的双方指定画像名称和 UID
ramaria import qq --file chat.txt \
  --persona-self-name "烧酒" \
  --persona-other-name "omkidaso" \
  --persona-other-uid "char-342215559"

# 跳过确认直接导入
ramaria import qq --file chat.txt --yes
```

| 参数 | 说明 |
|------|------|
| `--file <PATH>` | QQ 聊天记录文件路径（`.txt` 或 `.json`） |
| `--deep` | 深度导入模式：L0→L1→L2→L3 全管线 |
| `--persona <NAME>` | 导出者画像名称（向后兼容，等同于 `--persona-self-name`） |
| `--persona-self-name <NAME>` | 导出者画像名称 |
| `--persona-self-uid <UID>` | 导出者画像 UID（默认自动生成如 `char-{QQ号}`） |
| `--persona-other-name <NAME>` | 对方画像名称 |
| `--persona-other-uid <UID>` | 对方画像 UID |
| `--gap <MINUTES>` | 会话切割间隔（分钟），默认 1440（1 天） |
| `--yes` | 跳过诊断报告确认直接执行导入 |

**导入流程**：
1. 解析聊天记录文件（JSON 或 TXT 格式）
2. 显示诊断报告：消息数量、时间范围、参与者信息
3. 用户确认后执行导入：
   - 为双方自动创建 persona（source=`qq`）
   - 消息按发送者标记 `persona_uid`
   - 快速模式：仅写入 L0 + 生成 L1 摘要
   - 深度模式：L0→L1→L2→L3 全管线执行
4. 导入完成后显示统计报告

**支持的格式**：
- **JSON**：qq-chat-exporter v6.x 导出格式
- **TXT**：经典 PCQQ 导出 `.txt` 格式（GBK/UTF-8/UTF-16 多编码兼容）

---

### `ramaria diagnostics` — 诊断导出

导出诊断信息压缩包用于故障排查。

```
ramaria diagnostics
ramaria diagnostics --output ./my-diagnostics.zip
```

| 参数 | 说明 |
|------|------|
| `--output <PATH>` | 输出文件路径（默认 `ramaria-diagnostics-{时间戳}.zip`） |

**导出内容**：
- 最近 1000 行日志（`ramaria.log`）
- 配置文件（`config.toml`，API Key 已脱敏为 `[REDACTED]`）
- Schema 版本信息
- 操作系统信息（OS 名称、架构、内存等）

**安全保证**：
- API Key 在导出文件中不出现（全部替换为 `[REDACTED]`）
- 日志中的用户消息已截断和哈希化
- 输出路径有路径穿越防护（拒绝写入数据目录之外的路径）

---

### `ramaria export` — 数据导出

导出记忆和对话数据。

```
ramaria export                    # 导出为 JSON（默认）
ramaria export --format markdown  # 导出为 Markdown
ramaria export --output ./my-memories.json  # 指定输出文件
```

| 参数 | 说明 |
|------|------|
| `--format <json/markdown>` | 导出格式（默认 `json`） |
| `--output <PATH>` | 输出文件路径（默认 `./ramaria-export.{json,md}` 带时间戳） |

**导出内容**：
- **JSON**：完整结构化数据 — `sessions` → `messages` → `memory_l1` → `memory_events` → `personality_traits`
- **Markdown**：人类可读格式 — 按会话分组，包含消息对话、L1 摘要、L2 事件等

---

## 环境变量

| 变量 | 说明 |
|------|------|
| `RAMARIA_DATA_DIR` | 数据目录（覆盖默认 `%APPDATA%\Ramaria\`） |
| `RAMARIA_DB_PATH` | 数据库文件路径（覆盖 `--db` 选项） |
| `RUST_LOG` | 日志级别：`error` / `warn` / `info` / `debug` / `trace` |

---

## 错误处理

CLI 根据错误类型显示不同提示：

| 错误类别 | 典型提示 |
|----------|----------|
| `Config` | 配置缺失或格式错误，请运行 `ramaria setup` |
| `Storage` | 数据库不可用，请检查磁盘空间和权限 |
| `Llm` | LLM 调用失败，可重试 |
| `Privacy` | 线上 provider 需完成隐私确认 |
| `Index` | 索引损坏，请运行 `ramaria index rebuild` |
| `Validation` | 参数不合法（如 session 已关闭只读） |
| `Io` | 文件读写失败 |
| `Unsupported` | 不支持的导入格式 |

---

## 快速开始示例

```bash
# 1. 首次配置（选择 LM Studio 本地模型）
ramaria setup

# 2. 快速提问
ramaria ask "用一句话介绍自己"

# 3. 交互对话
ramaria chat

# 4. 查看记忆
ramaria memory --layer l1

# 5. 导入 QQ 聊天记录
ramaria import qq --file chat.json --deep

# 6. 导出诊断信息
ramaria diagnostics --output diag.zip

# 7. 导出数据
ramaria export --format markdown --output memories.md
```

---

## 命令变更

本版本（v1.5 M1）的 CLI 命名与输出约定变更，均保留旧命令兼容（clap alias），旧脚本不受影响：

| 变更 | 说明 |
|------|------|
| `utt` → `blocks` | 话语块命令 canonical 名称改为 `blocks`（如 `ramaria blocks rebuild`），`utt` 保留为 alias |
| `memory <层>` 层级别名 | 层级参数双支持：`l1`↔`summary`、`l2`↔`events`、`l3`↔`profile`；帮助与纠错提示同时列出 |
| 全局 `--json` | 所有命令支持信封输出；`ask --json` 修复为合法 JSON 事件流（`{"type":"delta|done|error",…}`），`--no-stream` 聚合为单个 `done` 事件。注意：事件流中输出 `error` 事件时进程仍以 exit 0 退出（错误已内嵌于流），需消费端检查事件类型，勿仅依赖 exit code |
| stdout/stderr 分离 | stdout 只输出数据；状态/提示/警告走 stderr（脚本依赖「stdout 混杂状态行」的需要调整） |
| exit code 约定 | `0` 成功 / `2` 参数错（clap）/ `3` LLM 或后端不可用 / `4` 业务校验失败；`--json` 模式错误信封的 `error.code` 复用该约定 |
| 时间戳约定 | `--json` 模式时间戳统一 ISO-8601 UTC（如 `2026-08-10T08:00:00Z`） |
| `persona list` | 新增：结构化列出人格（uid/名称/kind/来源/状态），支持 `--limit/--offset` 分页 |
| `status` | 新增：应用状态/配置摘要/DB 路径探活（agent 使用，非 TTY 可执行） |
| `import qq --dry-run` | 新增：仅解析预览输出结构化 JSON 摘要，不写入数据库（agent 先验证数据源）。预览 JSON 含双方 QQ uin/uid 标识字段（数据源验证用途），注意其隐私属性 |
| `session delete --force` | 新增：跳过确认（等同 `--yes` 双保险） |
| `memory` 默认 persona | 修正：`user-0001` 硬编码 → `rama-0001`（缺陷修复，查询默认对象变化） |

---

## 参考

- 桌面使用指南：`rust/docs/desktop-user-guide.md`
- 隐私说明：`rust/docs/privacy-notice.md`
- 完整架构说明：`rust/docs/dev/rust-rewrite-analysis.md`
- 默认配置模板：`rust/config/default.toml`
