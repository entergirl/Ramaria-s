# 珊瑚菌 · Ramaria v1.6

> 大模型懂一切，唯独不懂你。

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Repo](https://img.shields.io/badge/GitHub-entergirl%2FRamaria--s-black)](https://github.com/entergirl/Ramaria-s)

---

## 项目简介

现有 AI 助手存在一个根本性缺陷：没有记忆。每次对话都从零开始，无论之前交流过多少，下一次都是陌生人。

珊瑚菌以「记忆」为核心，构建一套**本地优先**的个人 AI 陪伴系统。它不单单记住"怎么说话"，更可以记住**你经历过什么、关心什么、思考问题的方式是什么**——包括那些在琐碎日常中一闪而过、连你自己都未必留意的瞬间。从这些微小的碎片里，它尝试勾勒出一个人的性格轮廓。

Ramaria 的核心能力：

- **分层记忆（L0→L3）**：从对话中自动提取摘要和事件。它关注藏在语气、措辞、反应模式中的线索——一次下意识的抱怨、一个反复出现的偏好、一句欲言又止——这些细微信号会被识别为可追溯的事件，成为推断性格的原始材料。不同层级之间树状关联，可以从一个性格标签一路回溯到当初那条对话原文。
- **自动人格推断**：把积累的事件当作一个人的行为样本，从中识别性格特征，让 AI 以这个人的身份和口吻与你对话。画像从真实对话数据中自然涌现，而非套用预设的性格模板。
- **三通道混合检索**：结合语义相似度、关键词精确匹配和知识图谱关联三种方式检索记忆，模拟人脑"记起一件事"的方式，既不会遗漏相近意思的表述，也不会错过关键事实。
- **数据完全本地**：所有对话和记忆存储在本地 SQLite 数据库，不上传任何服务器。使用本地 LM Studio 时可完全断网运行。
- **聊天记录导入**：支持导入 QQ 聊天记录，自动为参与对话的人建立独立的记忆和人格画像。
- **原生桌面应用**：基于 Tauri 2 构建，含系统托盘、通知、凭据管理器集成。
- **Rust 实现**：启动快、内存安全、CPU 和内存占用低。

---

## 与现有方案的本质差异

| 维度 | 微调方案 / LangMem | 本项目方案 |
|------|-------------------|-----------|
| 学习内容 | 语气、风格、措辞习惯 | 经历、习惯、情感纹理、思维方式 |
| 记忆方式 | 固化在模型权重中 / Prompt 注入 | 结构化存储，动态检索 + 遗忘曲线衰减 |
| 可解释性 | 黑盒，无法追溯 | 树状关联（L0→L1→L2→L3），可回溯至原始对话 |
| 更新机制 | 需重新训练 / 手动更新 | 实时写入，持续积累，全自动管线 |
| 隐私控制 | 数据上传至训练方 / 第三方 | 全量本地化，LM Studio 模式完全不联网 |
| 人格推断 | 固定 Prompt / 无 | 从对话事件中自动推断，Phase A/B/C 三级置信度 |
| 检索精度 | 向量 / 关键词单一通道 | 三通道融合（向量 + BM25 + 图谱）+ Persona-Aware |

---

## 系统架构

采用「本地推理 + 云端辅助」混合架构，SQLite 全部本地存储，LLM 调用对三后端透明。

```
Tauri 桌面应用 / CLI
    ↓↑ Command / 函数调用
ramaria-app  应用编排层（Pipeline + Stage 对话管线，状态机）
    ├── ramaria-memory     记忆管线（L0→L3）+ 混合 RAG + 性格推断（Phase A/B/C）
    ├── ramaria-llm        LLM Provider（LM Studio / DeepSeek / OpenAI）
    ├── ramaria-importer   聊天记录导入（QQ 解析 + 双模式导入）
    ├── ramaria-storage    SQLite（24 张表，Repository 模式）
    └── ramaria-core       核心类型 & Trait（零 I/O）
```

### 模块职责

| 模块 | 技术选型 | 职责 |
|------|----------|------|
| **ramaria-core** | 纯 Rust 类型系统 | 9 个枚举 + 9 个结构体 + StorageBackend trait（40+ 方法）+ LlmProvider trait |
| **ramaria-storage** | SQLite（sqlx） | 24 张表 schema、19 个 Repository、手动行映射避免 derive 侵入 |
| **ramaria-memory** | 自研管线 | 分层摘要→事件提取→性格推断、BM25+向量+图谱三通道 RAG、Ebbinghaus 衰减、RRF 融合、Token Budgeting |
| **ramaria-llm** | reqwest + SSE | 3 后端适配器、SSE 流式传输、API Key 凭据管理器、指数退避重试、ONNX 嵌入模型 |
| **ramaria-importer** | encoding_rs + sha2 | QQ 聊天记录解析（JSON + TXT）、快速/深度双模式、双画像自动创建 |
| **ramaria-app** | async-trait | CLI/Desktop 共用编排层，Pipeline+Stage 对话管线、状态机、隐私确认、流式事件模型、Session 生命周期管理、后台任务调度 |
| **ramaria-cli** | clap derive | 11 个子命令、交互式 REPL、色彩输出 |
| **ramaria-desktop** | Tauri 2 | 原生窗口、系统托盘、通知、CSP、Markdown 渲染、前端 JS（7 个视图） |

### 依赖关系（自底向上）

```
ramaria-core         零依赖，纯类型边界
  ├── ramaria-storage    依赖 core
  ├── ramaria-llm        依赖 core
  ├── ramaria-importer   依赖 core + storage
  └── ramaria-memory     依赖 core + storage
        └── ramaria-app        依赖 core + storage + memory + llm + importer
              ├── ramaria-cli       依赖 app + core + storage + llm
              └── ramaria-desktop   依赖 app + core + storage + llm
```

---

## 分层记忆体系

记忆体系的核心设计理念是：**不只是简单的逐层压缩，而是树状的、相互关联的有机网络**。不同层级之间通过数据库外键保持双向可追溯性。

```
L3 人格画像（长期稳定特征 + 置信度追踪）
    ↑ Phase A 统计推断 + Phase B LLM 推断 + Phase C 增量更新
L2 离散事件（8 个推断属性: confidence/valence/salience/attitude/paraphrase...）
    ↑ LLM 从 L1 中提取（未吸收 L1 ≥ 5 条 或 最早超 7 天触发）
L1 单次会话摘要（summary + keywords + time_period + atmosphere + 情感元数据 + situation_strength）
    ↑ LLM 从 L0 中压缩（session 关闭时触发）
L0 原始消息（永久保留，不删除，不过滤，标记发言人）
```

### L0 — 原始消息层

- 存储所有原始对话消息，永久保留
- 按发言人标记（user / assistant），按时间排序
- 细枝末节的日常闲聊同样写入，保留生活质感与情感纹理
- 作为 L1 摘要和事件提取的唯一可信数据源

### L1 — 单次会话摘要层

- **触发时机**：session 结束后自动触发（手动关闭或空闲 10 分钟自动关闭）
- **结构化字段**：摘要文本、关键词标签（从关键词池优先选择）、时间段（六选一）、对话氛围（四字以内）、情境强度（1-5 级）
- **情感元数据**：效价（valence, -1.0..1.0）和显著性（salience, 0.0..1.0）双字段，驱动记忆衰减差异化
- **关键词收敛**：优先复用 keyword_pool 中的已有词条，避免同义词膨胀

### L2 — 离散事件提取层

- **触发条件**：未吸收的 L1 ≥ 5 条，或最早未吸收 L1 超过 7 天（双路径触发）
- **事件属性**：title、summary、confidence、valence、salience、attitude、paraphrase、event_type
- **降级策略**：LLM 不可用时自动回退到规则式降级生成（combine summaries + 统计属性）
- L1 被吸收后以 lower_weight 保留，支持溯源与回滚

### L3 — 人格画像层

- **Phase A（统计推断）**：置信度 ≥ 0.6 的事件参与特征均值计算，situation_multiplier 情境加权
- **Phase B（LLM 推断）**：LLM 从事件聚类中提取抽象人格特征（trait_name + value + evidence + confidence）
- **Phase C（增量更新）**：定期对比新旧画像，计算 drift（漂移度），确认迁移路径
- **三层结构**：surface（说话风格）→ behavioral（行为模式）→ core（核心价值观），逐层置信度递增
- 画像支持 share 分级（private/trusted/public），控制 RAG 注入范围

---

## 核心创新设计

### 关键词词典收敛系统

传统方案每次摘要由模型自由发挥关键词，随时间积累会产生大量同义词，导致检索精度持续下降。

本项目维护 `keyword_pool` 词典表，每次 L1 生成时将历史词条作为候选列表喂给模型，引导模型优先复用已有词条——**让关键词随时间收敛而非发散**。

### 三通道混合 RAG 检索

```
用户消息 + persona_uid
  → Persona-Aware 过滤（按 share 分级过滤记忆）
  → 三通道并行检索：
      1. 向量通道（暴力搜索 BruteForceIndex）— 语义相似度
      2. BM25 通道（自研全文索引）— 关键词精确匹配
      3. 图谱通道（BFS 遍历实体关系）— 关联记忆召回
  → Token Budgeting（超出上下文窗口时在句子边界截断）
  → RRF（倒数排名融合）加权合并
  → Ebbinghaus 遗忘曲线衰减（salience 越高衰减越慢）
  → Top-K 注入 System Prompt
```

三通道互补设计：
- **向量通道**：捕捉语义相近但用词不同的记忆（如「不开心」匹配「沮丧」）
- **BM25 通道**：精确命中专有名词和事实（人名、地名、技术术语），弥补语义检索的不足
- **图谱通道**：召回与当前话题实体相关的历史事件（如聊到「Python」，召回之前学 Rust 时的挫折经历做关联）

### Ebbinghaus 遗忘曲线记忆衰减

记忆的检索权重随时间衰减，但受显著性（salience）调制：

| Salience | 衰减速度 | 典型场景 |
|----------|---------|---------|
| 0.9 | 极慢（50% 衰减需 ~30 天） | 重大人生事件、强烈情感体验 |
| 0.75 | 较慢（50% 衰减需 ~14 天） | 重要工作成果、意义深刻的对话 |
| 0.5 | 中等（50% 衰减需 ~7 天） | 日常学习讨论、一般兴趣话题 |
| 0.25 | 较快（50% 衰减需 ~3 天） | 琐碎事务、天气闲聊 |
| 0.0 | 纯 Ebbinghaus（~1 天） | 纯粹事务性对话 |

衰减曲线确保高情感密度的记忆保持更久，避免无关记忆占据检索窗口。

### 事件→性格推断管线

从离散事件中提取人格特征的全自动管线：

1. **示例精选器**：从事件池中按 salience 排序选择代表性样本
2. **聚类**：按主题聚类事件，消除孤例噪声
3. **LLM 推断**：对每个聚类请求 LLM 推断抽象人格特征
4. **校准**：按新鲜度 + 丰富度加权，确保推断不偏向单次异常事件
5. **漂移检测**：对比新旧画像，识别价值观迁移

### 隐私确认机制

首次使用线上 API（DeepSeek / OpenAI）时，弹出隐私确认提示，用户需手动输入 `yes` 确认。确认按 `provider + base_url` 粒度管理，切换 provider 时需重新确认。在 LM Studio（本地）模式下，该提示永不出现。

---

## 项目结构

```
crates/
├── ramaria-core/          # 核心类型 & Trait（零 I/O）
├── ramaria-storage/       # SQLite 存储层（24 张表，19 个 Repository）
├── ramaria-memory/        # 记忆管线 + RAG + 性格推断
├── ramaria-llm/           # LLM Provider（LM Studio / DeepSeek / OpenAI）
├── ramaria-importer/      # QQ 聊天记录导入
├── ramaria-app/           # 应用编排（Pipeline + Stage + Session 生命周期）
├── ramaria-cli/           # CLI 入口（11 个子命令）
└── ramaria-desktop/       # Tauri 2 桌面应用
config/
├── default.toml           # 默认配置模板
└── personas/              # 人格定义文件（TOML）
docs/                      # 用户文档
tests/                     # 集成测试
```

<details>
<summary>展开查看各 crate 内部目录</summary>

**ramaria-core/src/**
- `config.rs` — RamariaConfig 统一配置（11 个配置域）
- `error.rs` — 错误类型体系（8 种错误变体）
- `traits.rs` — StorageBackend（40+ 方法）、LlmProvider、EmbeddingProvider
- `types.rs` — 9 枚举 + 9 结构体（MemoryEvent, Persona, Session...）
- `keyword.rs` — KeywordToken / KeywordSet / KeywordStatus / KeywordRef

**ramaria-storage/src/**
- `database.rs` — 连接池管理 + migration runner
- `repo/` — 19 个 Repository（每类实体一个模块）

**ramaria-memory/src/**
- `l1/` — L0→L1 摘要生成（prompt + summarizer + evidence_notes）
- `event/` — L1→L2 事件提取 + TopicBatcher（batcher/mod,graph,buffer）+ ContextRetriever
- `keyword/` — BigramWithDictionaryNormalizer + AliasManager
- `inference/` — 性格推断 Phase A/B/C（stats, clustering, shrink, causal, inferrer, drift, confidence, orchestrator）
- `bm25.rs` / `vector.rs` / `graph_retriever.rs` — 三通道检索
- `retriever.rs` — Persona-Aware 混合检索器 + 增量索引
- `decay.rs` / `rrf.rs` / `token_budget.rs` — 衰减 / 融合 / 预算
- `prompt/` — 5-Block System Prompt 构建
- `init.rs` / `rebuild.rs` / `job.rs` — 冷启动 / 全量重建 / 后台任务

**ramaria-llm/src/**
- `provider.rs` — LlmProvider trait + 工厂模式
- `transport.rs` — SSE 流式传输 + 重试策略
- `lm_studio.rs` / `deepseek.rs` / `openai.rs` — 三后端适配器
- `keychain.rs` — OS 凭据管理器
- `embedding/` — ONNX 嵌入模型

**ramaria-importer/src/**
- `traits.rs` — ImportSource trait
- `qq/` — QQ 解析器 + 导入器（JSON + TXT）

**ramaria-app/src/**
- `app.rs` — App 状态机 + 核心编排
- `pipeline.rs` — PipelineStage trait + PipelineContext + 编排器
- `stages/` — 10 个 Pipeline Stage（check_state → persist_message）
- `app_chat.rs` / `app_retriever.rs` / `app_state.rs` — 对话编排 / 检索 / 状态
- `session_lifecycle/` — Session 生命周期（mod / idle / l1_generate / l2_l3_scheduler）
- `setup.rs` / `model_manager.rs` / `update.rs` / `diagnostics.rs` — 配置 / 模型 / 更新 / 诊断
- `stream_event.rs` / `privacy.rs` / `error_hint.rs` — 流式事件 / 隐私 / 错误映射

**ramaria-cli/src/**
- `main.rs` — clap 定义 + 全局选项
- `commands/` — 11 个子命令实现

**ramaria-desktop/**
- `src/` — Tauri Commands + 系统托盘 + 通知
- `frontend/` — 内嵌 Web UI（JS/CSS/HTML）
- `tauri.conf.json` — Tauri 配置（NSIS 打包）

</details>

**规模统计**：8 个 crate、~210 个源文件、~104,000 行 Rust 源码（含测试）、1590+ 测试函数。

---

## 快速开始

### 系统要求

- **Windows 11**（推荐）/ Windows 10
- 8 GB+ 内存
- 需要 LLM 后端（三选一）：
  - [LM Studio](https://lmstudio.ai/)（免费本地模型，推荐入门）
  - [DeepSeek API Key](https://platform.deepseek.com/)
  - [OpenAI API Key](https://platform.openai.com/)

### 桌面应用安装

1. 下载 `Ramaria_1.6.0_x64-setup.exe` 安装程序
2. 双击运行安装程序，按向导完成安装
3. 桌面出现 Ramaria 快捷方式，双击启动
4. 首次启动自动进入配置向导：
   - **LM Studio**：启动 LM Studio 并加载模型 → 填写 `http://localhost:1234/v1`
   - **DeepSeek / OpenAI**：输入 API Key（自动保存到 Windows 凭据管理器）
5. 配置完成后进入对话界面

> 详细指南见 [`docs/desktop-user-guide.md`](docs/desktop-user-guide.md)

### CLI 使用

```bash
# 首次配置向导
ramaria setup

# 单条提问（静默模式，非交互式）
ramaria ask "介绍一下你自己"

# 交互式对话（REPL）
ramaria chat

# 查看 L1 摘要
ramaria memory --layer l1

# 查看 L2 事件
ramaria memory --layer l2

# 查看 L3 人格画像
ramaria memory --layer l3

# 管理会话
ramaria session list
ramaria session show <id>
ramaria session delete <id>

# 修改配置
ramaria config set llm.provider deepseek
ramaria config set llm.api_key sk-xxxx

# 导入 QQ 聊天记录
ramaria import qq --file chat.json --deep

# 导出诊断信息
ramaria diagnostics --output diag.zip

# 数据导出
ramaria export --format markdown
ramaria export --format json

# 全局选项
ramaria --db ./custom.db ask "hello"     # 指定数据库路径
ramaria --yes ask "hello"                # 跳过隐私确认
```

> CLI 完整文档见 [`docs/cli-user-guide.md`](docs/cli-user-guide.md)

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [`docs/desktop-user-guide.md`](docs/desktop-user-guide.md) | 桌面应用完整使用指南（安装→配置→对话→记忆→人格→导入→诊断→故障排除） |
| [`docs/cli-user-guide.md`](docs/cli-user-guide.md) | CLI 命令参考（11 子命令 + 全局选项 + 环境变量） |
| [`docs/privacy-notice.md`](docs/privacy-notice.md) | 隐私说明（数据流向、API Key 安全、日志策略、CSP、导入数据说明） |
| [`config/default.toml`](config/default.toml) | 默认配置模板（含所有参数注释） |

---

## 隐私

Ramaria 默认将所有对话数据与记忆存储在本地。详见 [`docs/privacy-notice.md`](docs/privacy-notice.md)。

**核心要点**：

- **LM Studio 模式**：所有数据（对话 + 记忆）完全不出本地，不经过任何网络传输
- **线上 API**：对话内容发送至 API 服务器用于生成回复，历史记忆是否注入可控（可关闭 `online_memory_injection`）
- **API Key 安全**：通过 OS 凭据管理器存储（Windows Credential Manager），不写入明文配置文件
- **日志策略**：不记录完整对话内容，敏感信息（API Key、用户消息）截断或哈希化
- **导入数据**：QQ 聊天记录与本地对话同等对待，全部本地存储，不做匿名化
- **诊断导出**：打包日志和配置时 API Key 自动脱敏为 `[REDACTED]`
- **无遥测**：不包含任何数据收集、使用统计上报或网络回传
- **数据导出**：支持 JSON / Markdown 格式完全导出，支持数据库完整删除

---

## 版本历史

### v1.6.0（当前版本）

v1.6 是"知识深化"版本：让 persona 的事实性细节都能答上——把记忆里散落的碎片自动归纳成结构化知识卡片，越了解 ta、答得越准；画像也更准、更可解释。

**v1.6 新增**：

- 知识层：从聊天记录自动抽取并整理 ta 的事实（喜欢什么、家住哪、最近在忙什么），对话中问到相关话题时按需注入事实卡片
- 知识卡片：记忆页新增「知识」标签，展示分类整理的 ta 的事实，可查看历史版本（只读，保护隐私）
- 画像升级：画像推断更准——跨版本匹配阈值对齐、冷启动用跨用户经验校准、降级置信度可解释
- 隐私与降级：embedding/LLM 不可用时静默降级不阻塞；日志脱敏不记录原文/QQ 号/LLM 原始响应
- 探针自动评分：`probe evaluate/report` 自动给实验打分量、生成对比报告，为后续评估铺路
- 命令行：新增 `fact list/show`（知识事实查询）、`probe evaluate/report`；导入支持 `--side` 只处理某一侧
- 数据层：migrations 合并为单基线，`persona_facts` 版本化直建（**破坏性变更：旧库需重建**）
- 测试总数继续增长（分 crate 全绿，全量回归由负责人验收）

### v1.5.0 — 2026-08-15

"行为驱动"版本：让 persona 的反应模式自动复现——聊到特定话题时，ta 遇到工作吐槽时的典型反应、安慰人的方式自动流露。主要新增：行为规则自动学习（从对话归纳"什么情境怎么反应"，聊到时自动套用 ta 的习惯，规则可查看/编辑/禁用/手工导入并溯源到原事件）、导入进度可预期（实时显示"已完成 x/y · 预计剩余 N 分钟"）、三层生成缓存（重复请求不再重复计费）、上下文感知（摘要与回应体现话题延续与转折）、CLI 自动化友好改造（JSON 输出、命令命名规范化、`probe build/run` 探针工具链）、设置页视觉回归粉蓝双色设计系统。测试总数 ≥ 1590。

### v1.4.0 — 2026-08-08

v1.4 是"对话体感地基"版本：让对话回复开始"像 ta"——语气、口癖、原话直接复现；能答上 persona 的细节；隔天回来能接上上次聊的；空闲时长可自行设置。主要新增：utt 原文话语块（按话题注入 persona 原话）、examples 自学习（自动积累回复对作风格兜底）、会话桥接、evidence_notes 结构化线索、空闲时长滑动块 UI、四层注入骨架、设置功能完善（基础/高级两级设置页 + 配置双写同步），并修复真实消息导入测试发现的 6 项 P0/P1 缺陷。测试总数 ≥ 1200。

### v1.3.0 — 2026-07-22

算法深化版本：TopicBatcher 语义聚类分批、关键词体系 Newtype 升级、evidence_notes 双层摘要、CompositeIndex 三级上下文检索、Phase A 四因子校准权重链 + 三轨准入 + 分层收缩 + A8 因果链分析、底层动机标注激活、跨版本簇匹配、前端 L3 三层性格展示，以及全部 P0/P1 审查项修复和三轮测试缺陷修复。

### v1.2.0 — 2026-07-07

深度打磨版本：Pipeline + Stage 对话管线架构重构、L3 Phase B/C 管线贯通、前端 SessionDrawer + 记忆卡片情感可视化 + 记忆→对话跳转、Session-Persona 绑定、Retriever 增量索引。

### v1.1.0 — 2026-06-16

首个增量版本：Session 生命周期全自动级联触发、本地 ONNX 嵌入模型 + BM25 降级、情境强度加权、Token Budgeting、QQ 导入器、多角色管理 GUI。

### v1.0.1 — 2026-06-13

紧急修复：全新安装无法启动（Tauri 插件配置反序列化错误）。

### v1.0.0 — 2026-06-12

Rust 重写完成的首个正式发布版本。完整记忆管线（L0→L3）、事件→性格推断（Phase A/B/C）、三通道混合 RAG、LM Studio/DeepSeek/OpenAI 三后端、Tauri 2 桌面应用。

> 完整变更记录见 [CHANGELOG.md](CHANGELOG.md)。

### 与 Python 版（v0.7.x）的关系

Python 版位于项目根目录，已进入**维护模式**，不再活跃开发。Rust v1.5 是正式替代版本。

- 两个版本不共享数据库 schema，Python 旧数据不可自动迁移
- Python 版将继续保留于仓库，接收关键安全修复，但不增加新功能
- 新用户建议直接使用 Rust v1.5

> 两个版本使用独立的 Git 仓库管理历史和远程地址。

---

## 路线图

v1.5 已完成"行为驱动"（行为层全链路 + 驱动环接线 + 导入 ETA + 生成缓存 + 探针 CLI 骨架 + CLI 自动化改造）。后续按 2.0 路线推进（`docs/dev-2.0/roadmap.md`）：v1.6 知识深化（知识卡片 + 画像升级 + 探针自动评分 + utt/D 参数定稿）→ v1.7 风格与闭环（风格统计 + 反馈环 + 正式评估）→ 2.0 整合发布。

以下功能已在架构中预留接口，进入 **延后（deferred）** 队列，不在 v1.5 发布范围内：

- Ollama / Anthropic Claude / 通义千问后端
- 微信 / Telegram / Discord / Slack 导入器
- MCP Bridge（Python 版已有 MCP Server，Rust 版待适配）
- 自动下载安装更新（Tauri updater）
- ratatui TUI 模式
- SQLCipher 加密存储
- Portable 免安装模式
- 导入匿名化
- D3.js 知识图谱可视化
- LoRA 微调
- macOS / Linux 安装包
- 底层动机标注（✅ v1.3 已激活）
- 取消生成、编辑历史发言、重新生成、分支对话
- tiktoken-rs 精确 token 化
- Character Card JSON/PNG 导出

> 如有需求，请在 [GitHub Issues](https://github.com/entergirl/Ramaria-s/issues) 提出。

---

## 许可证

[MIT](LICENSE) © 2026 黎烧酒
