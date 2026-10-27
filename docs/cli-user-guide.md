# Ramaria CLI 使用指南（3.0）

> 版本：3.0
> 适用平台：Windows 首发（源码可在 macOS / Linux 构建）
> 读者：进阶用户、脚本与 Agent 开发者
> 相关文档：[桌面使用指南](desktop-user-guide.md)、[隐私说明](privacy-notice.md)、[默认配置模板](../config/default.toml)

`ramaria` 是 Ramaria 的命令行入口，覆盖首次配置、对话、记忆查询、导入导出、人格与规则管理、社交通道维护、MCP 服务端与评估探针，共 **21 个顶层子命令**。CLI 与桌面共用同一套服务层实现；默认数据目录相互独立，但可以用 `--db` 指向桌面的数据库共享同一份记忆。

---

## 1. 安装与运行

### 1.1 发行安装包不随带 CLI

Releases 页面的 Windows 安装包（`Ramaria_<版本>_x64-setup.exe`）只包含桌面应用，不附带 `ramaria` 命令行，也不会写入 PATH。使用 CLI 需要从源码构建，或取得已构建的二进制。

### 1.2 从源码构建

前置要求：

- Rust 1.88 及以上；
- Windows 需要 Visual Studio Build Tools（MSVC 工具链）；
- 无需 Node.js（前端构建不属于 CLI）。

```bash
cd <仓库根目录>/main
cargo build -p ramaria-cli
# 调试二进制：target/debug/ramaria.exe
# 发布构建：cargo build -p ramaria-cli --release
```

把 `target\debug` 或 `target\release` 加入 PATH，或创建 `ramaria.cmd` 快捷入口：

```bat
@echo off
"%~dp0target\debug\ramaria.exe" %*
```

### 1.3 建议先完成桌面首次配置

**3.0 已知限制**：`ramaria setup` 会写入后端配置与凭据，但不会写入桌面首次配置的完成标记（`settings.setup_completed_at`）。如果这个库从未在桌面走完三步配置向导，`ask` / `chat` 会被状态门禁拒绝（提示尚未就绪）。

因此，**全新库的首次使用请先在桌面应用完成配置向导**，再用 `--db` 指向同一个数据库：

```bash
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" status
```

只使用 CLI 完成首次配置并进行对话的 headless 场景暂不完整可用；导入、查询、导出等命令不受该门禁影响。该问题转 3.0.1 修复。已有桌面配置的库可以直接用 CLI 调整后端、导入、查询与导出。

---

## 2. 全局约定

### 2.1 全局选项

| 选项 | 说明 |
|------|------|
| `--db <PATH>` | 数据库文件路径。默认 `data/ramaria.db`（相对当前工作目录）；可用 `RAMARIA_DB_PATH` 覆盖。建议始终显式传入绝对路径。 |
| `--json` | 全局 JSON 信封：stdout 仅输出 `{"ok":true,"data":…}` / `{"ok":false,"error":{…}}`。`setup` / `chat` 为交互式命令，不支持该选项。 |
| `--yes` | 自动确认隐私、删除、导入等确认点；非 TTY 且未传 `--yes` 时不挂起，直接失败并提示。 |
| `--quiet` | 抑制 stderr 的 info / success / warn 提示，仅保留错误输出。 |
| `--skip-validate` | 跳过 LLM 连接验证，仅对 `setup` 生效。 |

`--db` 是全局选项，建议放在子命令之前：

```bash
ramaria --db "C:\path\to\ramaria.db" memory l1
```

### 2.2 输出与退出码

- stdout 只输出数据；状态、提示与警告走 stderr。
- 文本模式适合人读；`--json` 适合脚本与 Agent。
- 时间戳在 `--json` 中统一为 ISO-8601 UTC（如 `2026-10-27T08:00:00Z`）。
- 退出码：

| 退出码 | 含义 |
|---|---|
| `0` | 成功 |
| `2` | 参数错误（clap） |
| `3` | LLM / 嵌入 / 存储后端不可用（可重试类） |
| `4` | 业务校验失败（参数不合法、隐私拒绝、`--results` 缺失等） |
| `1` | 其它失败（配置、序列化、IO、导入格式等） |

`ask --json` 是**事件流**，不是全局信封：每行一个 `{"type":"delta|done|error",…}`。流中出现错误事件时进程会以非零码退出，消费端不要只看事件行。

### 2.3 环境变量

| 变量 | 作用 |
|---|---|
| `RAMARIA_DB_PATH` | 覆盖 CLI 的 `--db` 默认值（显式 `--db` 优先）。 |
| `RAMARIA_DATA_DIR` | 桌面应用的数据根目录；CLI 的 `persona reload` 也会在该目录下查找 `personas/`。它**不改变** CLI 的数据库路径。 |
| `RUST_LOG` | 日志级别：`error` / `warn` / `info` / `debug` / `trace`，默认 `info`。 |

---

## 3. 快速开始

### 3.1 已有桌面配置的库

```bash
# 探活：状态、provider、模型、数据库路径
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" status

# 单条提问
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" ask "用一句话介绍你自己"

# 交互式对话
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" chat

# 查看记忆
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" memory l1
```

### 3.2 全新库

请先在桌面完成配置向导，再按 3.1 使用。`ramaria setup` 仍可用于已有桌面配置的库（例如切换 provider、补充嵌入路径），但它不会替代桌面首次配置。

### 3.3 常用任务

```bash
# 导入 QQ 聊天记录
ramaria --db <库路径> import qq --file chat.json --deep

# 续跑中断的导入后处理
ramaria --db <库路径> import continue --export-root "C:\path\to\exports"

# 导出会话
ramaria --db <库路径> export markdown --output memories.md

# 诊断导出
ramaria --db <库路径> diagnostics --output diag.zip
```

---

## 4. 命令参考

### 4.1 对话

#### `ramaria setup` — 后端配置向导

交互式四步：选择 Provider → 填 Base URL → 选填嵌入模型路径 → 线上 provider 填 API Key；随后初始化人格并刷新状态。

```bash
ramaria --db <库路径> setup
ramaria --db <库路径> setup --skip-validate
```

- 不支持 `--json`。
- `--skip-validate` 跳过末尾的 LLM 连接验证；配置本身仍会保存。
- 不写桌面首次配置完成标记，限制见 §1.3。
- API Key 只进 Windows 凭据管理器，不写入 `config.toml`。

#### `ramaria ask` — 单条消息

```bash
ramaria ask "今天天气怎么样"
ramaria ask "介绍一下你自己" --persona rama-0001
ramaria ask "你好" --session <UUID>
ramaria ask "用一行话解释什么是 Rust" --no-stream
ramaria ask "今天心情如何" --json
```

| 参数 | 说明 |
|---|---|
| `<MESSAGE>` | 用户消息，可多词。 |
| `--persona <UID>` | 目标人格；不指定时用默认对话人格（缺省 `rama-0001`）。 |
| `--session <UUID>` | 复用已有会话；不指定则新建。 |
| `--no-stream` | 等待完整回复后一次性输出。 |
| `--json` | 事件流输出；`--no-stream --json` 聚合为单个 `done`。 |

默认流式；`||` 在文本模式下替换为换行。空回复按错误处理。

#### `ramaria chat` — 交互式 REPL

```bash
ramaria --db <库路径> chat
```

| 内置命令 | 作用 |
|---|---|
| `/exit`、`/quit`、`/q` | 退出；退出时关闭活跃会话并触发 L1 摘要 |
| `/save` | 保存并关闭当前会话，下一条消息自动开始新会话 |
| `/clear` | 清屏（不会新建会话） |
| `/help`、`/?` | 显示帮助 |

- 启动时创建新会话；空闲超过 `[session].l1_idle_minutes`（默认 10 分钟）后自动关闭并重建。
- 与桌面不同，CLI REPL 退出会关闭活跃会话；桌面退出不会强制封存。

### 4.2 记忆

#### `ramaria memory` — 查看分层记忆

```bash
ramaria memory                    # 默认 L1
ramaria memory l2                 # L2 事件
ramaria memory events             # L2 别名
ramaria memory l3                 # L3 画像
ramaria memory profile            # L3 别名
ramaria memory --persona rama-0001 --limit 20 --offset 0
```

- 层级别名：`l1`↔`summary`、`l2`↔`events`、`l3`↔`profile`。
- L1 时间行按**发生时间**显示；`--json` 条目含 `occurred_at`，并保留 `created_at`。
- `--limit` 1–500；`--offset` 用于分页。

#### `ramaria blocks` — 话语块管理（`utt` 为别名）

```bash
ramaria blocks rebuild
ramaria blocks rebuild --force
ramaria utt rebuild
```

- `rebuild`：增量重建全部会话的话语块，并刷新检索索引。
- `--force`：先清空全部旧块再按当前 `[utt]` 参数全量重切；修改切分参数后必须使用。
- `[utt].enabled = false` 时跳过重建并返回空统计。

#### `ramaria index` — 索引重建

```bash
ramaria index rebuild
ramaria index rebuild --json
```

- 在当前进程内同步重建 BM25、向量与图谱索引，完成后输出文档数与耗时（文档数为 L1 + L2，不含 utt 块）。
- 3.0 已删除 `AppState::Indexing`；索引状态由 `IndexStatus` 单独表达。桌面端的重建是后台任务并发送 `index-progress`，CLI 这里是同步命令。
- `--json` 输出 `doc_count` / `state` / `elapsed_ms`。

### 4.3 数据

#### `ramaria import` — 聊天记录导入

只支持 QQChatExporter v6.x 导出的 JSON；TXT、PCQQ 与 Telegram 导出不在 3.0 范围。

```bash
# 快速导入：L0 + 图片理解 + L1
ramaria import qq --file chat.json

# 深度导入：再加 L2 事件与 L3 画像级联
ramaria import qq --file chat.json --deep

# 仅解析预览，不写库
ramaria import qq --file chat.json --dry-run

# 指定双方画像
ramaria import qq --file chat.json \
  --persona-self-name "我的昵称" \
  --persona-other-name "对方昵称" \
  --persona-other-uid "char-123456789"

# 只处理某一侧
ramaria import qq --file chat.json --side other
```

| 参数 | 说明 |
|---|---|
| `--file <PATH>` | 导出 JSON 路径；支持中文路径。 |
| `--deep` | 深度导入：L0 → 图片理解 → L1 → L2 → L3。 |
| `--dry-run` | 只解析并输出结构化预览，不写数据库。 |
| `--persona <NAME>` | 导出者画像名称（向后兼容，等同 `--persona-self-name`）。 |
| `--persona-self-name <NAME>` | 导出者画像名称。 |
| `--persona-self-uid <UID>` | 导出者 UID；缺省按 `user-` 前缀生成。 |
| `--persona-other-name <NAME>` | 对方画像名称。 |
| `--persona-other-uid <UID>` | 对方 UID；缺省按 `char-` 前缀生成。 |
| `--side <self\|other\|both>` | 导入侧过滤，默认 `both`。 |
| `--gap <MINUTES>` | 会话切割间隔，默认 10 分钟。 |
| `--no-report` | 不输出解析报告（报告含导出者 / 对方标识，默认掩码版）。 |
| `--force` | 跳过确认（等同 `--yes` 双保险）。 |

导入流程：解析 → 报告 → 确认 → L0 写入 → 导入后处理（图片理解 → L1 → 深度级联）。重复消息按导入指纹去重。图片理解能力不可达时附件保持待处理，可稍后续跑。

#### `ramaria import continue` — 续跑导入后处理

```bash
ramaria import continue --export-root "C:\path\to\exports"
ramaria import continue --export-root "D:\exports" --export-root "E:\exports" --yes
ramaria import continue --no-reset-skipped --limit 100 --no-cascade
```

| 参数 | 说明 |
|---|---|
| `--export-root <PATH>` | 导出目录，可重复传入；用于图片定位与按 md5 反查。缺省时跳过附件回补，仅补 L1 与级联。 |
| `--no-reset-skipped` | 不重置历史「已跳过」附件，只处理当前待处理项。 |
| `--limit <N>` | 本次最多补跑的会话数；`0` = 不限。 |
| `--no-cascade` | L1 补跑后不触发 L2/L3 级联。 |
| `--yes` | 跳过确认。 |

可重复执行；已完成的图片理解与摘要会自动跳过。

#### `ramaria export` — 导出会话

```bash
ramaria export                    # JSON，默认路径 exports/export_<时间戳>.json
ramaria export markdown           # Markdown（也接受 md）
ramaria export --persona rama-0001 --output ./memories.json
ramaria export --redact --output ./redacted.json
```

| 参数 | 说明 |
|---|---|
| `[format]` | 位置参数：`json` / `markdown` / `md`，默认 `json`。 |
| `--persona <UID>` | 按人格筛选会话，并附加该人格的未吸收 L1 摘要段。 |
| `--output <PATH>` | 输出路径；缺省 `exports/export_<时间戳>.<ext>`；`-` = stdout；裸文件名按当前目录处理。 |
| `--redact` | 消息正文与 L1 摘要替换为 `<N chars>`，其余字段不变。 |

**导出范围**：会话与消息；`--persona` 时附 L1 摘要。**不包含** L2/L3、知识事实、人格定义与向量。要完整备份，请复制 `ramaria.db` 与相关目录。

### 4.4 管理

#### `ramaria session` — 会话管理

```bash
ramaria session list --limit 20 --offset 0
ramaria session show <UUID>
ramaria session delete <UUID> --force
ramaria session summarize <UUID> --progressive
```

| 子命令 | 说明 |
|---|---|
| `list` | 列出会话、时间、消息数；活跃会话单独标注。 |
| `show` | 按时间顺序显示消息。 |
| `delete` | 删除会话及其消息、关联 L1；不可逆，非 TTY 需 `--yes` 或 `--force`。 |
| `summarize` | 为指定会话重新生成 L1；`--persona` 可选，`--progressive` 走渐进式重摘要（未触发阈值时回退单段）。 |

#### `ramaria config` — 配置管理

```bash
ramaria config list
ramaria config get provider
ramaria config set provider deepseek
ramaria config set base_url https://api.deepseek.com/v1
ramaria config set temperature 0.7
ramaria config set max_tokens 2048
ramaria config set api_key sk-...
ramaria config set embedding_model_path "D:\models\bge-small-zh-v1.5"
ramaria config set utt.theta_gap_minutes 30
```

| 子命令 | 说明 |
|---|---|
| `list` | 显示后端配置、状态与 settings 表；API Key 遮蔽。 |
| `get <KEY>` | 支持 `provider` / `base_url` / `model_id` / `temperature` / `max_tokens` / `api_key` / `state` / `embedding_model_path`，以及 `utt.*` / `bridge.*` 与 settings 键。 |
| `set <KEY> <VALUE>` | 支持 `provider` / `base_url` / `model_id` / `temperature` / `max_tokens` / `api_key` / `embedding_model_path`，以及 `utt.*` / `bridge.*` 与已存在的 settings 键。 |

- `api_key` 只写 Windows 凭据管理器，不落配置文件；本地 provider 会提示无需 API Key。
- `utt.*` / `bridge.*` 走文件 + DB 双写；桌面端需重启应用。
- 切换 provider 后，线上 provider 需重新完成隐私确认。

#### `ramaria persona` — 人格管理

```bash
ramaria persona list
ramaria persona show
ramaria persona reload
ramaria persona reload --uid rama-0001
ramaria persona import preset.toml --dry-run
ramaria persona import preset.json --into rama-0001 --no-ai
ramaria persona export rama-0001 --out rama.toml
ramaria persona export rama-0001 --with-examples
ramaria persona prompt rama-0001
ramaria persona prompt rama-0001 --input "今天有点累"
ramaria persona allow-inference rama-0001 --label openness
```

| 子命令 | 说明 |
|---|---|
| `list` | 结构化列人格（uid / 名称 / kind / 来源），支持 `--limit` / `--offset`。 |
| `show` | 显示全部人格摘要。 |
| `reload` | 扫描 `personas/` 目录同步到 DB；`--uid` 只重载一个。 |
| `import` | 导入 persona.toml 超集或 SillyTavern 卡（`.json` / `.png`）；`--dry-run` 预览、`--into` 合并、`--no-ai` 关闭自由文本拆解。 |
| `export` | 导出人格为预设 TOML；默认不含对话示例，`--with-examples` 才包含真实示例。 |
| `prompt` | 只读查看 System Prompt；缺省为静态段，`--input` 走含召回的全量预览。 |
| `allow-inference` | 允许推断管线接管该人格的手工画像；`--label` 可重复，缺省全部。 |

- 导入不保证 LLM 拆解成功：拆解失败或隐私未确认时，原文保留在人格配置里，人格照常建成。
- 桌面端的人格文件在安装资源 `config/personas`；CLI `reload` 按 `RAMARIA_DATA_DIR/personas`、`config/personas` 的顺序探测。

#### `ramaria rule` — 行为规则

```bash
ramaria rule list --persona rama-0001
ramaria rule show 3
ramaria rule import rules.json
ramaria rule edit 3 --reaction "先共情再给建议" --avoid "敷衍,讲大道理"
ramaria rule enable 3
ramaria rule disable 3
ramaria rule delete 3 --force
ramaria rule evidence 3
ramaria rule relearn --persona rama-0001
ramaria rule clusters --persona rama-0001 --theta-nb 0.6
```

- `delete` 是 CLI 独有的破坏性操作；3.0 桌面规则页不提供删除与重新推断。
- `edit` / `disable` 会写 `feedback_log`；`relearn` 重新聚类并替换 Auto 规则；`clusters` 只读，可用 `--theta-nb` / `--min-cluster-size` / `--beta1` / `--beta2` / `--theta-join` 做档位摸底。
- 规则文本与证据链均为脱敏内容，不落原文。

#### `ramaria fact` — 知识事实

```bash
ramaria fact list --persona char-0001 --field interests
ramaria fact show 3
```

只读查询 active 事实与版本链；**不提供删除**（CLI 与桌面一致）。

#### `ramaria keyword` — 关键词池

```bash
ramaria keyword list
ramaria keyword show "加班"
ramaria keyword seed "加班" "吐槽"
ramaria keyword alias list
ramaria keyword alias confirm "摸鱼"
ramaria keyword alias reject "摸鱼"
```

`keyword_pool` 有三态：规范词 / 已确认别名 / 待确认别名。待确认别名由关键词建议用例登记，`alias list` 可能为空；`seed` 幂等，不递增使用次数。

#### `ramaria style` — 表达层风格统计

```bash
ramaria style update --persona char-0001
```

基于该人格全部消息重算五维风格统计并生成 / 替换自动风格规则；样本不足时标注「数据不足」。

#### `ramaria channel` — 社交通道维护

```bash
ramaria --db <库路径> channel status
ramaria --db <库路径> channel bindings
ramaria --db <库路径> channel bind qq c2c:10001 --persona rama-0001 --enable --allow-proactive
```

- `status`：总开关 / 平台开关 / WS 地址 / 凭据是否已配置；**运行连接态只在桌面可见**。
- `bindings`：列出外部会话 ↔ 本机人格 / 启用 / 允许主动。
- `bind`：写入或更新绑定；`--persona` 缺省保持现值，`--enable` / `--disable` 与 `--allow-proactive` / `--deny-proactive` 成对互斥。
- 凭据不提供 CLI 明文入口，请在桌面「设置 → 社交与接入」填写；连接由桌面托管，CLI 不建立连接。

#### `ramaria sticker` — 表情资产

```bash
ramaria --db <库路径> sticker add smile.png --description "开心" --tag 开心 --tag 摸鱼
ramaria --db <库路径> sticker list
ramaria --db <库路径> sticker update 1 --description "坏笑" --tag 摸鱼
ramaria --db <库路径> sticker remove 1
```

- 资产文件复制到数据库同级的 `stickers/`；`remove` 同时删除文件。
- 单张资产上限 20 MB；社交出站图片上限 8 MB；未命中降级为纯文字。
- 本地对话不使用表情资产；Telegram 出站附件未实现（表情按文本占位降级）。
- 3.0 没有表情管理界面，仅 CLI。

#### `ramaria diagnostics` — 诊断导出

```bash
ramaria diagnostics
ramaria diagnostics --output .\diag.zip
ramaria diagnostics --redact --output .\diag.zip
```

- 打包最近 1000 行日志、`config.toml`、schema 信息与系统信息。
- 始终脱敏：API Key 替换为 `[REDACTED]`；日志中的消息类字段替换为字符数；绝对路径只留文件名。
- `--redact` 为脚本统一传参兼容，幂等。
- 输出路径会做规范化与符号链接检查，拒绝根目录 / 盘符裸路径。

### 4.5 高级

#### `ramaria status` — 状态探活

```bash
ramaria status
ramaria status --json
```

输出应用状态、provider、Base URL、模型、温度、最大输出 token、数据库路径与版本；不访问 LLM，不触发网络。

#### `ramaria mcp` — MCP 服务端

```bash
ramaria --db "%APPDATA%\Ramaria\data\ramaria.db" mcp serve
```

- 以 stdio 启动 MCP 服务端，七个工具：`memory_recall` / `chat_send` / `chat_ingest` / `persona_list` / `persona_get` / `chat_history` / `privacy_confirm`。
- 门禁取自 `[mcp]`：`enabled`（默认关闭）/ `allow_ingest` / `allow_seal` / `allowed_personas` / `allow_raw_text` / `max_items` / `max_chars`。
- stdout 只承载协议消息，日志走 stderr；退出时最多等待在途任务 15 秒。
- `--db` 建议放在 `mcp` 之前；客户端配置片段可在桌面「设置 → 社交与接入 → MCP」一键复制。
- 配置或凭据改动后需重启客户端连接。

#### `ramaria probe` — 评估探针（开发者）

探针用于离线评估，不参与日常使用。数据集与报告均为本地文件；`probe run` 会真实写库、真实调用 LLM。

```bash
ramaria probe build --output probe-dataset.json
ramaria probe run --dataset probe-dataset.json --output probe-results.json --json
ramaria probe evaluate --results probe-results.json --dataset probe-dataset.json --output eval.json
ramaria probe report --results probe-results.json --evaluation eval.json --output report.md
ramaria probe baseline --window-hours 24 --json
```

| 子命令 | 用途 | 关键参数 |
|---|---|---|
| `build` | 从导入数据构建三维测试集（语气 / 事实 / 情感） | `--persona` / `--questions-per-dim` / `--seed` / `--source` / `--output` |
| `run` | 按档位批量运行对话管线 | `--dataset` / `--variants` / `--limit` / `--rebuild-utt` / `--no-rebuild-utt` / `--repeat` |
| `evaluate` | 事实维 golden + 语气维本地 judge + 情感维 rubric | `--results` / `--dataset` / `--no-tone-judge` |
| `report` | 档位对比 / 消融报告 | `--evaluation` / `--calibration` / `--ablation` |
| `baseline` | 主动对话投递 / 回应 / 判据计数，只读 | `--window-hours`（`0` = 不限） |

注意：

- `--rebuild-utt` 会按档位参数清空并重建数据库中的 utt 块；建议对副本库运行。
- `evaluate` 的语气维只在本地 LM Studio 上运行，线上后端自动跳过。
- `probe run` 会真实写会话与消息；线上 provider 需隐私确认（`--yes`）。
- `probe evaluate` / `report` 缺少 `--results` 时退出码为 `4`。

探针的完整档位、消融与统计口径见项目技术报告（`对话-事件-人格画像系统技术报告`）。

---

## 5. 数据与配置

### 5.1 数据库与目录

| 项 | CLI 默认 | 桌面默认 |
|---|---|---|
| 数据库 | `data/ramaria.db`（相对 CWD） | `%APPDATA%\Ramaria\data\ramaria.db` |
| 配置 | 数据库同目录 `config.toml` | `%APPDATA%\Ramaria\data\config.toml` |
| 日志 | stderr（CLI 不写桌面日志文件） | `%APPDATA%\Ramaria\data\logs\ramaria.log` |
| 凭据 | Windows 凭据管理器 | Windows 凭据管理器 |

- CLI 与桌面默认独立；用 `--db` 指向桌面库即可共享会话、记忆与人格。
- `config.toml` 与 DB 双写，以文件为准；CLI 启动时会做一致性校验并回写 DB。
- API Key 只进 Windows 凭据管理器，不写入 `config.toml`、不进入日志。

### 5.2 3.0 重建库

3.0 是单基线、无升级器：不读取 2.x 的 `assistant.db`，也不提供迁移命令。

1. 备份旧库与 `config.toml`；需要时先 `export` / `diagnostics`。
2. 删除目标 `ramaria.db`（或换 `--db` 路径）；首次启动自动按基线建库。
3. 重新导入 QQ 记录，按需执行 `blocks rebuild` / `rule relearn` / `style update`。
4. 核对消息、L1、L2、L3、知识与导入统计。

> 2.x 的 `assistant.db` 可保留作备份，3.0 不会读取或删除。
>
> 若这是 CLI 首次接触该库，按 §1.3 先在桌面完成配置向导，CLI 的 `ask` / `chat` 才会放行。

---

## 6. 兼容与别名

| 变更 | 说明 |
|---|---|
| `utt` → `blocks` | canonical 名称是 `blocks`（如 `ramaria blocks rebuild`），`utt` 保留为别名。 |
| `memory` 层级别名 | `l1`↔`summary`、`l2`↔`events`、`l3`↔`profile`。 |
| `probe dataset` → `probe build` | `dataset` 保留为别名。 |
| `--json` | 全局信封；`ask --json` 是事件流；`setup` / `chat` 不支持。 |
| 导出参数 | 3.0 桌面导出参数改 camelCase（`outputPath`）；CLI 仍用 `--output`。 |
| 库文件 | `assistant.db` → `ramaria.db`；不提供升级。 |
| 索引状态 | 删除 `AppState::Indexing`；桌面用 `IndexStatus` + `index-progress`，CLI `index rebuild` 同步执行。 |
| 线上记忆注入 | `[backend].online_memory_injection` 已删除；线上后端恒注入记忆。 |
| 关键词层 | `[retrieval].enable_keyword_fuzzy` 独立控制关键词语义层；`enable_vector` 只控文档向量。 |

---

## 7. 故障排查

| 现象 | 处理 |
|---|---|
| `ask` / `chat` 提示尚未就绪 | 该库未完成桌面首次配置；先在桌面走完三步向导，或用已配置的库。见 §1.3。 |
| `ramaria` 命令找不到 | 把 `target\debug` / `target\release` 加入 PATH；安装包不随带 CLI。 |
| MCP 客户端没有工具 | 确认 `ramaria` 在 PATH；确认 `[mcp].enabled` 已打开；重启客户端。 |
| MCP 召回为空 | 配置片段必须含 `--db` 桌面库路径；客户端工作目录不确定，缺省会打开另一份库。 |
| 社交消息无回复 | 检查桌面通道是否启动、绑定是否放行；群聊需 @ 或回复；凭据改动后重新启停通道。 |
| 导出报路径错误 | 裸文件名按当前目录处理；检查父目录是否存在、是否有写权限；拒绝根目录 / 盘符裸路径。 |
| 导入解析失败 | 只支持 QQChatExporter v6.x JSON；先 `--dry-run` 验证数据源。 |
| 索引重建很慢 | CLI 为同步全量重建；大库建议在桌面端用后台重建，或对副本库操作。 |
| 探针结果缺失 | `probe evaluate` / `report` 缺 `--results` 返回 4；检查参数与文件路径。 |

---

## 8. 参考

- [桌面使用指南](desktop-user-guide.md)
- [隐私说明](privacy-notice.md)
- [默认配置模板](../config/default.toml)
- [新手引导](getting-started.md)
- 问题反馈：[GitHub Issues](https://github.com/entergirl/Ramaria-s/issues)
