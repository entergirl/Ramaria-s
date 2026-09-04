# Ramaria CLI 使用指南

> 版本：v1.4（v1.3 + M5 规则管理命令 `rule`、行为层说明更新）
> 适用平台：Windows / macOS / Linux（Windows 首发）

## 概述

`ramaria` 是 Ramaria 的命令行入口，支持对话、记忆查询、会话管理、配置修改、人格管理、行为规则管理（rule）、数据导入、诊断导出、数据导出与探针实验（probe）。

首次使用前需运行首次配置向导（`ramaria setup`）。**CLI 与桌面应用的数据目录相互独立**：CLI 的数据库由 `--db` 指定（默认 `data/ramaria_assistant.db`，`RAMARIA_DB_PATH` 覆盖）；桌面应用开发模式（`cargo tauri dev`）使用 `crates/ramaria-desktop/.ramaria-dev/`、生产模式使用 `%APPDATA%\Ramaria\data\`；API key 均保存在 Windows Credential Manager（不落盘）。

---

## 安装与运行

**无需安装**：仓库已构建的调试二进制位于 `target\debug\ramaria.exe`（仓库根下），包含全部命令（含 probe），直接执行即可。建议先 `cd` 到仓库根目录再运行，避免相对路径（默认数据库 `data/ramaria_assistant.db`）跑偏；也可创建 `ramaria.cmd` 快捷入口（内容：`@echo off` + `"%~dp0target\debug\ramaria.exe" %*`），之后在仓库根目录下直接敲 `ramaria`。

### 从安装包

Windows 安装包将 `ramaria.exe` 安装到系统 PATH。

### 从源码编译

```bash
cd <仓库根目录>
cargo build -p ramaria-cli
# 调试二进制位于 target/debug/ramaria.exe
```

---

## 全局选项

所有子命令都支持以下全局选项：

| 选项 | 说明 |
|------|------|
| `--db <PATH>` | 数据库文件路径。默认 `data/ramaria_assistant.db`，可通过 `RAMARIA_DB_PATH` 环境变量覆盖 |
| `--json` | 全局 JSON 信封输出：stdout 仅输出 `{"ok":true,"data":…}` / `{"ok":false,"error":{...}}`，供脚本/agent 解析（遵循全局 JSON 信封约定） |
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
ramaria memory l2                 # 查看 L2 事件
ramaria memory events             # 层级别名：l2 ↔ events
ramaria memory l3                 # 查看 L3 性格画像
ramaria memory profile            # 层级别名：l3 ↔ profile
ramaria memory --persona rama-0001  # 筛选特定人格
ramaria memory --limit 20         # 限制返回条数
```

| 参数 | 说明 |
|------|------|
| `<LAYER>` | 记忆层级位置参数：`l1`/`summary`（摘要，默认）、`l2`/`events`（事件）、`l3`/`profile`（性格画像），层级别名双支持，未知层级有纠错提示 |
| `--persona <UID>` | 按人格筛选 |
| `--limit <N>` | 返回条数限制 |
| `--offset <N>` | 跳过前 N 条（分页） |

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
  --persona-self-name "我的昵称" \
  --persona-other-name "对方昵称" \
  --persona-other-uid "char-123456789"

# 跳过确认直接导入
ramaria import qq --file chat.json --yes

# 只处理对方消息（跳过"我方"侧，my persona 不创建）
ramaria import qq --file chat.json --side other
```

| 参数 | 说明 |
|------|------|
| `--file <PATH>` | QQ 聊天记录文件路径（`.json`；TXT 格式支持规划中） |
| `--deep` | 深度导入模式：L0→L1→L2→L3 全管线 |
| `--persona <NAME>` | 导出者画像名称（向后兼容，等同于 `--persona-self-name`） |
| `--persona-self-name <NAME>` | 导出者画像名称 |
| `--persona-self-uid <UID>` | 导出者画像 UID（默认自动生成，**我方 `user-` 前缀 / kind=user**，如 `user-123456789`） |
| `--persona-other-name <NAME>` | 对方画像名称 |
| `--persona-other-uid <UID>` | 对方画像 UID（默认 `char-` 前缀 / kind=char，如 `char-123456789`） |
| `--side <self\|other\|both>` | 导入侧过滤，默认 `both`：`self` 只处理"我方"（导出者，kind=user）、`other` 只处理对方（kind=char）、`both` 全部处理；`*-uid` 前补 `user-`/`char-` 前缀也按侧归一 |
| `--gap <MINUTES>` | 会话切割间隔（分钟），默认 10 |
| `--yes` | 跳过诊断报告确认直接执行导入 |

**导入流程**：
1. 解析聊天记录文件（JSON 格式；TXT 格式支持规划中）
2. 显示诊断报告：消息数量、时间范围、参与者信息
3. 用户确认后执行导入：
   - 为双方自动创建 persona（source=`qq`）
   - 消息按发送者标记 `persona_uid`
   - 快速模式：仅写入 L0 + 生成 L1 摘要
   - 深度模式：L0→L1→L2→L3 全管线执行
4. 导入完成后显示统计报告

**支持的格式**：
- **JSON**：qq-chat-exporter v6.x 导出格式（TXT / PCQQ 格式支持规划中）

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

### `ramaria probe` — 探针实验（v1.5 M2 新增；v1.7 扩展）

自动化工具链：构建测试集 + 按参数档位批量跑对话管线，用于 utt 参数定稿（θ_gap / 条数上限 / top_k）与聚类参数摸底、消融评估（M5a/M5b）。建立于 M1 `--json` 信封约定之上；v1.7 起探针规模扩展为 3 维、支持统计法（`--repeat`）与消融档位（`ablation` Profile）与消融对比报告（`report --ablation`）。utt 参数已定稿（θ_gap=10 / 条数 80 / top_k=3，写默认配置）；定稿结论与评估报告见 `docs/dev-1.7/`（`v1.5-probe-report.md` 转正式 + `test/probe-test-report-J-v17-20260903.md`）。

#### `ramaria probe build` — 构建测试集（旧名 `probe dataset`，保留 alias）

从导入数据自动构建测试集（3 维「语气模仿 tone / 事实记忆 fact / 情感表达 emotion」× 每维 10 题），输出结构化 JSON 数据集。

```
ramaria probe build --output probe-dataset.json
ramaria probe build --persona char-0001 --questions-per-dim 15 --seed 42 --json
ramaria probe build --source custom-source.json --output ds.json
ramaria probe dataset --output ds.json        # 旧名 alias，等价
```

| 参数 | 说明 |
|------|------|
| `--persona <UID>` | 目标 persona（默认自动选白名单内角色类 persona，兜底 `char-0001`） |
| `--questions-per-dim <N>` | 每维题数（默认 10，共 20 题；v1.7 正式评估 ≥30 题时调大） |
| `--seed <N>` | 抽样 seed（默认 `20260810`，固定可复跑：同 seed 输出相同测试集） |
| `--source <FILE>` | 显式数据源文件（JSON：`{persona_uid, messages:[{question,reply,source_ref}], events:[{title,summary}]}`）；不指定则从数据库构建 |
| `--output <FILE>` | 数据集输出文件（`-` = stdout）；不指定时 `--json` 输出完整数据集到 stdout |

**降级**：数据库无真实数据 / 数据源文件处理失败时，自动以内置测试夹具数据兜底（记 warn，不报错），保证测试集恒有 `2 × 每维题数` 道题（真实数据在前，夹具补齐在后，每题 `source` 字段标注 `db`/`file`/`fixture`）。

**档位组合**（代表配对，写进数据集供 run 使用）：

| 档位 id | θ_gap（分钟） | 条数上限 | top_k | 说明 |
|---------|:---:|:---:|:---:|------|
| baseline | 30 | 40 | 3 | v3.1 初值（对照基准） |
| theta_gap_60 | 60 | 40 | 3 | θ_gap 上调（块更长） |
| max_msgs_80 | 30 | 80 | 3 | 条数上限上调（块更长） |
| top_k_1 | 30 | 40 | 1 | top_k 下调（更保守的原文注入） |

> 以上为 M1 utt 参数定稿实验的代表配对（v1.5 M2 遗留的对照档位）。utt 参数已定稿为 θ_gap=10 / 条数=80 / top_k=3（写默认配置）。**消融档位**（v1.7 M5）：数据集 `variants[]` 可含 `ablation` 字段（取值 `B0/B1/F0/F1/F2/F3/F4/S_behavior/S_knowledge/S_expression/S_narrative`），运行时对每档真实关闭/保留对应记忆注入层（`ablation` 缺失 = 完整体系，与 M1 行为一致）；`ramaria probe` 无独立 profile flag——档位由数据集文件携带，供 `evaluate`/`report --ablation` 配对对比。

#### `ramaria probe run` — 档位批量实验

按档位批量跑对话管线，结构化输出（档位 → 输出 → 指标），供 v1.6 T2 自动评分与 v1.7 T3 正式评估复用。

```
ramaria probe run --dataset probe-dataset.json --output probe-results.json --json
ramaria probe run --dataset ds.json --variants baseline,top_k_1 --limit 10 --json
ramaria probe run --dataset ds.json --no-rebuild-utt --json
```

| 参数 | 说明 |
|------|------|
| `--dataset <FILE>` | 数据集文件（`probe build` 产物），必选 |
| `--variants <ids>` | 只跑指定档位（逗号分隔 id，默认全部；无效 id 忽略） |
| `--limit <N>` | 每档位最多跑题数（默认全部） |
| `--rebuild-utt` / `--no-rebuild-utt` | 是否按档位参数重建 utt 块（默认开启；θ_gap/条数档位必须重建才生效。注意：开启会**清空并按新参数重建数据库中的 utt 块**） |
| `--repeat <N>` | 统计法重复次数（v1.7 新增，默认 1）：同一档位跑 N 轮，输出含 `repeat.per_variant` 逐题统计与每轮 `rounds` 全量明细（供 evaluate 逐轮评分聚合）；`variants` 保留最后一轮单次快照。解决 DeepSeek 无 `seed` 复跑不一致（D-V17-001 统计法） |
| `--output <FILE>` | 结果输出文件（`-` = stdout 输出原始结果 JSON） |
| `--json` | M1 信封输出（`{"ok":true,"data":{…}}`） |

**输出结构**（信封 `data`）：
- `variants[]`：每档位 `variant_id` / `params`（三参数）/ `runs[]`（每题 `item_id` / `dimension` / `question` / `reply` / `metrics{reply_chars,elapsed_ms}` / `error`）/ `failed_count`
- 单题失败不中断批量：失败题 `error` 记录原因，其余题与档位继续执行

**注意**：
- 线上 provider（DeepSeek/OpenAI）需要隐私确认，脚本场景加 `--yes`
- 每次对话会新建会话并写库（探针实验数据）；建议对副本数据库执行
- 隐私红线：日志不记录完整问题与回复；数据集含参考文本（persona 原回复/事件摘要），注意保管

#### `ramaria probe evaluate` — 对档位实验自动评分（v1.6新增；v1.7 扩展）

对 `probe run` 结果自动打分：事实维 golden answers（embedding 余弦 + 关键词命中加权）、语气维 LLM-as-judge（本地 LM Studio，rubric 1~5 分，温度 0，**线上后端自动跳过**）、情感维（v1.7 新增，确定性 rubric 0/0.5/1——情境极性 × 安慰/共情/喜悦标记计数，非事实召回）。服务于知识层抽取质量评估（漏报目标 <10%）与 v1.7 正式评估铺路。

```
ramaria probe evaluate --results probe-results.json --dataset probe-dataset.json --output eval.json --json
ramaria probe evaluate --results probe-results.json --no-tone-judge --output eval.json
```

| 参数 | 说明 |
|------|------|
| `--results <FILE>` | 实验结果文件（`probe run` 产物），必选 |
| `--dataset <FILE>` | 数据集文件（`probe build` 产物；提供时按 golden `reference` 精确评分，缺失退化为问题文本近似） |
| `--variants <ids>` | 只评指定档位（逗号分隔 id，默认全部） |
| `--output <FILE>` | 评分数值文件（`-` = stdout 输出评分 JSON） |
| `--no-tone-judge` | 跳过语气维 LLM-as-judge（无本地 LM Studio 或想省时使用） |
| `--json` | 信封输出（`{"ok":true,"data":{…}}`） |

**输出结构**：`ProbeEvaluation`——档位 × 维度评分（`fact_score`/`tone_score`/`emotion_score`），逐题 `FactItemScore`（余弦/关键词命中/加权）；judge 不可用时 `judge_used=false` 标注，不阻断 fact 维评分。

**逐轮聚合（v1.7）**：当 `probe run` 用了 `--repeat N` 且结果含 `repeat.per_variant[].rounds` 时，`evaluate` 对每轮 reply 分别评分，聚合为 `dimension_scores[].{mean,std,ci95_low,ci95_high,n}`（n=轮数，t 分布置信区间）；`variants`（最后一轮）保留为单次快照不参与聚合。无 repeat 的旧结果行为不变。

> **exit code**：`probe evaluate` / `probe report` 缺 `--results` 时返回 **exit code 4**（业务校验失败，v1.7 由 1 修正）。

#### `ramaria probe report` — 生成档位对比报告（v1.6新增；v1.7 消融对比）

根据 evaluate 评分与人工抽检校准生成档位对比表与定稿建议（markdown/JSON 双形态，按输出扩展名 `.md`/`.json` 分派）。v1.7 起支持 `--ablation` 消融对比模式。

```
ramaria probe report --results probe-results.json --evaluation eval.json --output report.md
ramaria probe report --results probe-results.json --evaluation eval.json --calibration manual.json --output report.json
ramaria probe report --results probe-results.json --evaluation eval.json --ablation --output report-ablation.md
```

| 参数 | 说明 |
|------|------|
| `--results <FILE>` | 实验结果文件（`probe run` 产物），必选 |
| `--evaluation <FILE>` | 评分数值文件（`probe evaluate` 产物） |
| `--calibration <FILE>` | 人工抽检校准文件（JSON 数组 `{item_id, score}`；10%~20% 抽样与 judge 比对） |
| `--output <FILE>` | 报告输出文件（`-` = stdout；`.md` markdown / `.json` JSON） |
| `--ablation` | 消融对比报告模式（v1.7 新增，需提供 `--evaluation`，缺失 exit 4） |
| `--json` | 信封输出 |

**校准**：`read_manual_scores` 读取人工分数，`compute_calibration` 计算同分一致性 / 平均绝对差 / 偏差 / 校准系数；同分 <50% 或 `|偏差|>1.0` 时报告标注不一致，供人工复核。

**`--ablation` 消融对比（v1.7）**：自动识别 F0（F 组）与 B1（S 组）基线，按题目配对执行 Wilcoxon 符号秩检验（正态近似、平均秩，n<5 返回 None 保守处理）+ Cohen's d + 95% CI + 全局 Benjamini-Hochberg FDR 校正；判定线 **p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0 → 显著**（↑ 正向 / ↓ 负向 / → 无差异）；附辅助指标表（平均回复字符 / 耗时 / 空回复率）。对照档位由数据集 `variants[].ablation` 给出。

---

### `ramaria fact` — 知识层事实查询（v1.6新增）

查看 persona 的知识事实（v3.1 §5 知识层）——从事件抽取并仲裁后的结构化事实，按 ProfileField 分组，含历史版本链。**仅查询命令，双端均无 delete**（D-V16-003）。

```
ramaria fact list                              # 列出默认 persona（rama-0001）的 active 事实
ramaria fact list --persona char-0001 --json   # 指定 persona + JSON 信封
ramaria fact list --field interests            # 按 field 过滤
ramaria fact show 3                            # 查看事实 #3 详情（含版本链）
```

| 子命令 | 说明 |
|--------|------|
| `list` | 列出 persona 的 active 事实（按 field 分组：basic_info/personal_status/interests/social/history/recent_context/speaking_style）；`--persona` 过滤、`--field` 过滤、`--limit/--offset` 分页、`--json` 信封输出 |
| `show <ID>` | 单条事实详情（content/confidence/source/keyword_hint）+ 完整版本链（`version_of` 历史版本折叠展示） |

**事实卡片字段**：`field`（类别徽标）、`content`（事实陈述，**非原文**）、`confidence`、`source`（event/manual）、`keyword_hint`。仅展示 `status=active` 事实；历史版本沿 `version_of` 链查看。**无 delete 命令**（双端一致，D-V16-003）。

---

### `ramaria rule` — 行为规则管理（v1.5 M5 新增）

行为层（情境-反应规则）的管理命令：查看 persona 自动学习到的行为规则、手工导入、编辑/启用/禁用/删除，以及查看规则证据链（规则 → 事件 → 原文摘要溯源）。建立于 M1 `--json` 信封约定与 §2.9 动词词表之上；行为层设计见算法说明书 v3.1 §4 与 `docs/dev-1.5/v1.5-plan.md` §2.5。

```
ramaria rule list                              # 列出默认 persona（rama-0001）的规则
ramaria rule list --persona char-0001 --json   # 指定 persona + JSON 信封
ramaria rule show 3                            # 查看规则 #3 详情
ramaria rule import rules.json                 # 手工导入规则（JSON 文件，- = stdin）
ramaria rule edit 3 --reaction "..." --avoid "a,b"
ramaria rule enable 3 / ramaria rule disable 3
ramaria rule delete 3                          # 破坏性操作：交互确认 / --yes 自动通过
ramaria rule evidence 3                        # 规则 → 事件 → 原文摘要溯源链
```

| 子命令 | 说明 |
|--------|------|
| `list` | 列出 persona 的行为规则（默认 `rama-0001`；`--persona` 筛选、`--limit/--offset` 分页，`--json` 输出含分页前 `total`） |
| `show <ID>` | 查看单条规则详情（来源 Manual/Auto、状态、规则文本、情境关键词、情感强度/主动程度/详细度/正式度、禁忌列表、置信度/稳定性/证据数） |
| `import <FILE>` | 手工导入规则 JSON（`-` = stdin），导入后 `source=Manual` 自动生效；宽松 situation 解析、空情境/空规则拒绝 |
| `edit <ID>` | 编辑规则（`--reaction` / `--avoid`，至少一个；编辑后规则转为 Manual 并写 S1 反馈日志） |
| `enable <ID>` / `disable <ID>` | 启用 / 禁用规则（disable 写 S1 反馈日志） |
| `delete <ID>` | 删除规则（破坏性操作：交互确认，非 TTY 或 `--yes/--force` 自动通过） |
| `evidence <ID>` | 展示规则证据链：规则 → 事件（title/摘要/paraphrase，脱敏字段，原文不落日志） |

**手工导入 JSON 格式**（宽松解析，字段缺失取默认值）：

```json
{
  "situation": {"keywords": ["工作", "加班", "吐槽"]},
  "reaction": "遇到工作吐槽时，先共情再给实用建议",
  "params": {"emotional_intensity": 0.6, "proactiveness": 0.5, "detail_level": 0.5, "formality": 0.3},
  "avoid": ["敷衍安慰", "讲大道理"]
}
```

**注意**：
- 规则文本与 evidence 均为脱敏内容（paraphrase/摘要），原始对话不落日志（隐私红线）
- 删除为破坏性操作，脚本场景加 `--yes` 或 `--force`
- 编辑/禁用会写入 `feedback_log`（S1 强信号，weight=1.0），用于 v1.7 反馈环（H2）校准

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
ramaria memory l1

# 5. 导入 QQ 聊天记录
ramaria import qq --file chat.json --deep

# 6. 导出诊断信息
ramaria diagnostics --output diag.zip

# 7. 导出数据
ramaria export --format markdown --output memories.md

# 8. 构建探针测试集（2 维 × 10 题，seed 固定可复跑）
ramaria probe build --output probe-dataset.json

# 9. 跑档位实验（默认 4 档位；--rebuild-utt 会按档位参数重建 utt 块）
ramaria probe run --dataset probe-dataset.json --output probe-results.json --json
```

---

## 命令变更

本版本（v1.5）的 CLI 命名与输出约定变更，均保留旧命令兼容（clap alias），旧脚本不受影响（M1 自动化友好改造 + M2 探针命令）：

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
| `probe dataset` → `probe build` | **M2 新增探针命令**：`probe build`（构建测试集）/ `probe run`（档位批量实验），`dataset` 保留为 alias；详见上文 `probe` 章节 |
| `rule list/show/import/edit/enable/disable/delete/evidence` | **M5 新增行为规则管理命令**（v1.5 规则管理决策：UI 延后，仅后端 + CLI）；详见上文 `rule` 章节 |
| `probe evaluate/report` | **v1.6 M4 新增探针评分命令**：`probe evaluate`（事实维 golden + 语气维 LLM-as-judge 自动评分）/ `probe report`（档位对比报告 + 定稿建议）；详见上文 `probe` 章节 |
| `import qq --side` | **v1.6 M0 新增导入侧过滤**：`self\|other\|both`（默认 both），只处理某一侧时跳过侧消息不入库、该侧 persona 不创建（D-V16-011） |
| `fact list/show` | **v1.6 M1 新增知识层查询命令**：查看 persona 结构化事实与版本链；**双端均无 delete**（D-V16-003）；详见上文 `fact` 章节 |
| 默认画像 UID 语义 | **v1.6 M0 修正"我方/对方"数据库对齐（D-V16-011）**：新导入自动生成的"我方"画像 UID 前缀 `user-` / kind=user（对方仍 `char-` / kind=char）；旧库需按 v1.6 重建库升级路径，无数据回填 |
| `probe run --repeat N` | **v1.7 M1 新增统计法**：同一档位多次运行取均值 ± 置信区间（D-V17-001，解决 DeepSeek 复跑不一致） |
| `probe run --no-rebuild-utt` | **v1.7 M1 新增**：非切分档位复用已建 utt 块（embedding 调用数不随档位倍增） |
| `probe build` 3 维 | **v1.7 M5a 扩展**：新增情感表达 emotion 维（qpd×3，30 题时共 90 题） |
| `probe report --ablation` | **v1.7 M5a 新增消融对比报告**：配对 Wilcoxon + Cohen's d + CI + BH-FDR 判定 |
| `probe evaluate/report` exit code | **v1.7 M1 修正**：`--results` 缺失时 exit code 由 1 修正为 4（业务校验失败语义） |
| 风格规则注入 | **v1.7 M2 新增（A3）**：自动风格统计生成 SpeakingStyle 规则注入对话 prompt（`[style]` 配置，关闭回退 v1.6）；无需新 CLI 命令 |
| 弱反馈闭环 | **v1.7 M4 新增（H2）**：S2/S3 弱信号检测写入 `feedback_log`（默认仅审计不自动改动）；无需新 CLI 命令 |
| `import qq --file` 中文路径 | **v1.7 M0 修复**：文件路径参数改 `PathBuf`/`OsString` 承载，中文文件名导入冒烟通过 |

---

## 版本升级与重建库

> **v1.7 无破坏性变更**：从 v1.6.0 升级到 v1.7.0 **无需重建库**。v1.7 仅新增 `persona_style_stats` 增量表（migration 只增不删），`feedback_log`/`persona_facts`/`behavior_rules` 结构不变；风格注入/渐进式摘要/脉络加权/弱反馈均为独立配置开关（默认值见 `config/default.toml` 的 `[style]`/`[l1.progressive]`/`[retrieval] narrative_weighted`/`[feedback]`），关闭即回退 v1.6 行为。旧库直接启动自动应用增量 migration 即可。

> **v1.6 破坏性变更（D-V16-014）**：所有 migration 已合并为单个 `20260815_v1.6_schema.sql` 基线，`persona_facts` 以版本化结构（status/tier/version_of/confidence/keyword_hint）直建。旧版数据库的 `_sqlx_migrations` 记录与该基线 checksum 不匹配，**无法自动迁移，需重建库**。

从 v1.5（及更早）升级到 v1.6.0 的流程：

1. **备份**：先备份旧库数据（如 `data/ramaria_assistant.db`），导出诊断/记忆以防意外。
2. **重建**：删除旧库文件，作为全新库启动（v1.6 首次启动自动 `migrate!` 建新 schema）。
3. **重新导入**：重新导入外部聊天记录（`ramaria import qq --file ... [--side self|other|both]`），重新生成 L1/L2/L3 与知识事实。v1.5 的三层生成精确缓存（`llm_response_cache`/`l2_cluster_fingerprints`）可复用，降低重新生成成本。
4. **关键数据核对**：核对消息量、事件数、画像、知识事实与导入统计一致后投入使用。

> 说明：项目当前无存量用户、v1.5 未正式发布，此刻整理成本最低；除本文外，`docs/dev-1.6/v1.6-decisions.md`（D-V16-014）与 `CHANGELOG.md` 亦标注了该破坏性变更。

---

---

## 参考

- 桌面使用指南：`docs/desktop-user-guide.md`
- 隐私说明：`docs/privacy-notice.md`
- 探针实验设计与定稿：`docs/dev-1.5/v1.5-probe-report.md`（v1.5 计划/决策/任务清单同目录）
- 默认配置模板：`config/default.toml`
