# 珊瑚菌 · Ramaria v1.2

> 大模型懂一切，唯独不懂你。

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Repo](https://img.shields.io/badge/GitHub-entergirl%2FRamaria--s-black)](https://github.com/entergirl/Ramaria-s)

---

## 项目简介

现有 AI 助手存在一个根本性缺陷：没有记忆。每次对话都从零开始，无论之前交流过多少，下一次都是陌生人。

珊瑚菌以「记忆」为核心，构建一套**本地优先**的个人 AI 陪伴系统。它不只记住「怎么说话」，而是记住**你经历过什么、关心什么、思考问题的方式是什么**。

与普通 AI 聊天应用不同，Ramaria 具备：

- 🧠 **分层记忆体系（L0→L3）**：从对话中自动提取摘要、事件、性格画像，树状关联，可回溯至原始对话
- 🎭 **人格画像自动推断**：从对话事件中识别对话对象的性格特征，LLM 以此人格身份进行对话
- 🔍 **三通道混合 RAG 检索**：向量检索 + BM25 关键词 + 知识图谱，Persona-Aware 过滤，RRF 融合 + Ebbinghaus 遗忘曲线衰减
- 🔒 **数据完全本地化**：所有记忆存储在本地 SQLite，不上传任何服务器；线上 API 调用可控可关闭
- 📥 **聊天记录导入**：支持 QQ 聊天记录导入（JSON + TXT），快速/深度双模式
- 🖥️ **原生桌面应用**：基于 Tauri 2 构建，含系统托盘、通知、凭据管理器集成，性能优异
- ⚙️ **Rust 实现**：零 GC 停顿，内存安全，启动快，CPU/内存占用低

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
    ├── ramaria-storage    SQLite（23 张表，Repository 模式）
    └── ramaria-core       核心类型 & Trait（零 I/O）
```

### 模块职责

| 模块 | 技术选型 | 职责 |
|------|----------|------|
| **ramaria-core** | 纯 Rust 类型系统 | 9 个枚举 + 9 个结构体 + StorageBackend trait（40+ 方法）+ LlmProvider trait |
| **ramaria-storage** | SQLite（sqlx） | 23 张表 schema、19 个 Repository、手动行映射避免 derive 侵入 |
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
├── crates/
│   ├── ramaria-core/             # 核心类型 & Trait（零 I/O）
│   │   └── src/
│   │       ├── config.rs         # RamariaConfig 统一配置（10 个配置域）
│   │       ├── error.rs          # 错误类型体系（8 种错误变体）
│   │       ├── traits.rs         # StorageBackend（40+ 方法）、LlmProvider、EmbeddingProvider
│   │       └── types.rs          # 9 枚举 + 9 结构体（MemoryEvent, Persona, Session...）
│   ├── ramaria-storage/          # SQLite 存储层
│   │   └── src/
│   │       ├── database.rs       # 连接池管理 + migration runner
│   │       └── repo/             # 19 个 Repository（每类实体一个模块）
│   ├── ramaria-memory/           # 记忆系统核心
│   │   └── src/
│   │       ├── l1/               # L0→L1 摘要生成（prompt + summarizer）
│   │       ├── event/            # L1→L2 事件提取 + 降级策略
│   │       ├── inference/        # 性格推断 Phase A/B/C（统计/聚类/推断/置信度/漂移）
│   │       │   └── orchestrator.rs # Phase B/C 编排函数（v1.2 新增）
│   │       ├── prompt/           # System Prompt 5-Block 构建
│   │       ├── bm25.rs           # BM25 全文索引
│   │       ├── vector.rs         # BruteForceIndex 暴力向量索引
│   │       ├── graph_retriever.rs # 知识图谱 BFS 遍历检索
│   │       ├── decay.rs          # Ebbinghaus 衰减函数
│   │       ├── rrf.rs            # 倒数排名融合算法
│   │       ├── retriever.rs      # Persona-Aware 混合检索器（含 LRU 淘汰）
│   │       ├── token_budget.rs   # Token 计数与预算分配
│   │       ├── init.rs           # 冷启动流程（首次加载人格 + 知识注入）
│   │       ├── rebuild.rs        # 全量重建（重新提取 L1/L2/L3）
│   │       └── job.rs            # 后台任务调度（含 CancellationToken）
│   ├── ramaria-llm/              # LLM Provider 层
│   │   └── src/
│   │       ├── provider.rs       # LlmProvider trait + 工厂模式
│   │       ├── transport.rs      # SSE 流式传输 + 重试策略
│   │       ├── keychain.rs       # OS 凭据管理器（Windows Credential Manager）
│   │       ├── lm_studio.rs      # LM Studio 适配器
│   │       ├── deepseek.rs       # DeepSeek API 适配器
│   │       ├── openai.rs         # OpenAI API 适配器
│   │       └── embedding/        # ONNX 嵌入模型（feature gate `embedding-onnx`）
│   ├── ramaria-importer/         # 聊天记录导入（v1.1 新增）
│   │   └── src/
│   │       ├── traits.rs         # ImportSource trait
│   │       └── qq/               # QQ 解析器 + 导入器（JSON + TXT 双格式）
│   ├── ramaria-app/              # 应用编排层（CLI/Desktop 共用）
│   │   └── src/
│   │       ├── app.rs            # App 状态机 + 核心编排
│   │       ├── pipeline.rs       # PipelineStage trait + PipelineContext + 编排器（v1.2 新增）
│   │       ├── stages/           # 10 个 Pipeline Stage（v1.2 新增）
│   │       │   ├── check_state.rs       # Stage 1: 状态检查
│   │       │   ├── check_privacy.rs     # Stage 2: 隐私确认
│   │       │   ├── resolve_session.rs   # Stage 3: 会话管理 + persona_uid 绑定
│   │       │   ├── load_history.rs      # Stage 4: 历史消息 + L1 上下文
│   │       │   ├── retrieve_memory.rs   # Stage 5: RAG 三通道检索
│   │       │   ├── build_prompt.rs      # Stage 6: 5-Block System Prompt
│   │       │   ├── token_budget.rs      # Stage 7: Token 预算
│   │       │   ├── build_request.rs     # Stage 8: ChatRequest 构造
│   │       │   ├── call_llm.rs          # Stage 9: LLM 流式调用
│   │       │   └── persist_message.rs   # Stage 10: 消息保存 + 事件转发
│   │       ├── app_chat.rs       # 对话编排（委托 Pipeline）
│   │       ├── app_retriever.rs  # 检索编排
│   │       ├── app_state.rs      # 状态管理
│   │       ├── session_lifecycle.rs # Session 生命周期 + L0→L3 级联
│   │       ├── setup.rs          # 首次配置向导
│   │       ├── model_manager.rs  # 嵌入模型下载管理
│   │       ├── update.rs         # 自动更新检查
│   │       ├── diagnostics.rs    # 诊断导出
│   │       ├── stream_event.rs   # 流式事件模型
│   │       ├── privacy.rs        # 隐私确认管理
│   │       └── error_hint.rs     # 错误到用户友好提示的映射
│   ├── ramaria-cli/              # 命令行入口
│   │   └── src/
│   │       ├── main.rs           # clap 定义 + 全局选项
│   │       ├── commands/         # 11 个子命令实现
│   │       ├── ui.rs             # 交互式 REPL 界面
│   │       └── util.rs           # TOML 解析 + 工具函数
│   └── ramaria-desktop/          # Tauri 桌面应用
│       ├── src/
│       │   ├── main.rs           # Tauri 入口
│       │   ├── commands/         # Tauri Command 定义（委托给 ramaria-app）
│       │   ├── tray.rs           # 系统托盘（图标 + 菜单）
│       │   ├── notification.rs   # 通知推送
│       │   └── events.rs         # 事件处理
│       ├── frontend/             # 内嵌 Web UI（JSON over Tauri IPC）
│       │   ├── index.html
│       │   ├── css/
│       │   └── js/
│       └── tauri.conf.json       # Tauri 配置（NSIS 打包）
├── config/
│   ├── default.toml              # 默认配置模板（v1.1 新增）
│   └── personas/                 # 人格定义文件（TOML）
│       └── rama-0001.toml        # 默认人格「黎杋枫」
├── docs/                         # 用户文档
│   ├── desktop-user-guide.md     # 桌面使用指南
│   ├── cli-user-guide.md         # CLI 使用指南
│   └── privacy-notice.md         # 隐私说明
├── tests/                        # 集成测试
│   ├── integration_tests.rs      # 跨 crate 集成测试（13 个）
│   └── fixtures/                 # 测试 fixture（对话数据、事件 JSON）
└── Cargo.toml                    # Workspace 定义（resolver="3", edition="2024"）
```

**规模统计**：8 个 crate、~150 个源文件、~32,000+ 行 Rust 代码、~600 个测试函数。

---

## 数据库结构

本地 SQLite 数据库（默认路径 `%APPDATA%\Ramaria\data\assistant.db`），23 张表，按层次组织：

### 公共层

| 表名 | 用途 |
|------|------|
| `schema_meta` | 数据库 schema 版本、索引版本 |
| `personas` | 统一人格注册中心（uid/name/kind/source/ref_id/config/description） |

### L0 层

| 表名 | 用途 |
|------|------|
| `sessions` | 对话 session 生命周期管理（含 `persona_uid` 绑定，v1.2 新增） |
| `messages` | 原始消息流（含 persona_uid 发言人标记、import_fingerprint 去重） |

### L1 层

| 表名 | 用途 |
|------|------|
| `memory_l1` | 单次会话摘要（含 valence/salience/situation_strength/context_json） |

### L2 层（事件层）

| 表名 | 用途 |
|------|------|
| `memory_events` | 离散事件主表（含 8 个推断属性 + `motives` 预埋列（v1.3 激活）+ absorbed 标记） |
| `event_relations` | 事件网状关系（6 种关系类型） |
| `event_sources` | 事件溯源（关联事件→L1） |
| `persona_facts` | 原子化事实库 |
| `trait_evidence` | 性格证据链（支撑/矛盾关系） |
| `persona_cluster_snapshots` | 态度聚类快照 |

### L3 层（性格层）

| 表名 | 用途 |
|------|------|
| `personality_traits` | 三层结构化性格画像（含 confidence/evidence/consistency） |
| `persona_examples` | Few-shot 对话示例 |

### 基础设施层

| 表名 | 用途 |
|------|------|
| `keyword_pool` | 关键词词典（含同义词管理） |
| `bm25_index` | BM25 token 持久化 |
| `graph_nodes` | 知识图谱实体节点 |
| `graph_edges` | 知识图谱关系边 |
| `privacy_consent` | 隐私确认记录（按 provider + base_url 粒度） |
| `backend_config` | 非敏感后端配置 |
| `background_jobs` | 后台任务状态与重试 |
| `conflict_queue` | 冲突检测待确认队列 |
| `pending_push` | 主动推送消息暂存 |
| `settings` | 全局运行配置（key-value） |

> 注：Python 版的 `memory_l2`、`l2_sources`、`user_profile` 三张表已被 `memory_events` + `event_relations` + `event_sources` + `persona_facts` + `personality_traits` 替代。

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

1. 下载 `Ramaria_1.2.0_x64-setup.exe` 安装程序
2. 双击运行安装程序，按向导完成安装
3. 桌面出现 Ramaria 快捷方式，双击启动
4. 首次启动自动进入配置向导：
   - **LM Studio**：启动 LM Studio 并加载模型 → 填写 `http://localhost:1234/v1`
   - **DeepSeek / OpenAI**：输入 API Key（自动保存到 Windows 凭据管理器）
5. 配置完成后进入对话界面

> 📖 详细指南见 [`docs/desktop-user-guide.md`](docs/desktop-user-guide.md)

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

> 📖 CLI 完整文档见 [`docs/cli-user-guide.md`](docs/cli-user-guide.md)

---

## 开发

### 构建与测试

```bash
# 编译（release）
cargo build --release

# 运行全部测试
cargo test --workspace

# 代码质量检查
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# 桌面应用开发模式（热重载）
cd crates/ramaria-desktop
cargo tauri dev

# CLI 开发模式
cargo run -p ramaria-cli -- ask "hello"

# 构建 Windows 安装包
cd crates/ramaria-desktop
cargo tauri build
```

### Crate 总览

| Crate | 职责 | 依赖 | 测试数 |
|-------|------|------|--------|
| `ramaria-core` | 核心类型 & Trait | 零 I/O 依赖 | ~59 |
| `ramaria-storage` | SQLite 存储层 | core | ~36 |
| `ramaria-memory` | 记忆管线 + RAG + 推断 | core + storage | ~440 |
| `ramaria-llm` | LLM Provider + Embedding | core | ~43 |
| `ramaria-importer` | QQ 聊天记录导入 | core + storage | ~17 |
| `ramaria-app` | 应用编排（含 Pipeline + Stage） | core + storage + memory + llm | ~140 |
| `ramaria-cli` | 命令行入口 | app + core + storage + llm | ~40 |
| `ramaria-desktop` | Tauri 桌面应用 | app + core + storage + llm | ~16 |

### 技术栈

| 领域 | 技术 |
|------|------|
| 语言 | Rust（edition 2024，MSRV 1.85） |
| 异步运行时 | Tokio（full features） |
| 序列化 | Serde + serde_json |
| 数据库 | SQLite（sqlx 0.8） |
| CLI | clap 4（derive 模式） |
| 桌面 | Tauri 2 |
| 日志 | tracing + tracing-subscriber |
| 网络 | reqwest（JSON + SSE streaming） |
| 向量检索 | BruteForceIndex 暴力余弦 + 本地 ONNX 嵌入 |
| 文本编码 | encoding_rs（GBK/UTF-16 兼容） |
| 错误处理 | thiserror + anyhow |

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

### v1.2.0（当前版本）

v1.2 是深度打磨版本，聚焦架构健康度和功能完整性。在 v1.1 基础上完成了对话系统 Pipeline + Stage 架构重构、L3 记忆管线 Phase B/C 贯通、前端记忆与对话联动、以及多项后端缺陷修复。

**v1.2 新增**：

- ✅ Pipeline + Stage 架构重构：`send_message` 10 步管线拆解为独立可测试的 Stage（≥ 60 个新增测试）
- ✅ Session-Persona 绑定：`sessions` 表新增 `persona_uid`；用户消息 persona 归属统一
- ✅ L3 管线贯通：Phase B LLM 结构化推断 + Phase C 置信度更新全流程接通（≥ 37 个新增测试）
- ✅ SessionDrawer 组件：对话页左侧会话历史抽屉，搜索过滤 + 状态区分 + 会话切换
- ✅ L1 记忆卡片跳转：卡片"💬 查看对话"按钮 → 跳转对话 + 面包屑返回
- ✅ L1 卡片 UI 重新设计：valence 情感色条 + chip 关键词 + 属性行 + 操作栏
- ✅ 后端修复：空闲/shutdown 关闭时 `persona_uid` 正确传递；L1 生成后 Retriever 增量索引
- ✅ 导入进度 UI 增强：进度条放大 + 会话计数 + 预估剩余时间
- ✅ 测试总数 ≥ 600（v1.1: 546，新增 ≥ 50 个）
- ✅ Schema 预埋：`memory_events.motives` 列（v1.3 激活）

### v1.1.0 — 2026-06-16

Rust v1.0 的首个增量版本。补齐了 Session 生命周期管理、嵌入模型、情境强度加权、Token Budgeting、QQ 导入器等关键功能。

### v1.0.1 — 2026-06-13

紧急修复：全新安装无法启动（Tauri 插件配置反序列化错误）。

### v1.0.0 — 2026-06-12

Rust 重写完成的首个正式发布版本。与 Python v0.7.x 相比，Rust v1.0 重新设计了架构，不兼容旧数据库 schema。

**核心能力**：完整记忆管线（L0→L3）、事件→性格推断（Phase A/B/C）、三通道混合 RAG、LM Studio/DeepSeek/OpenAI 三后端、Tauri 2 桌面应用、CLI 9 个子命令。

### 与 Python 版（v0.7.x）的关系

Python 版位于项目根目录，已进入**维护模式**，不再活跃开发。Rust v1.2 是正式替代版本。

- 两个版本不共享数据库 schema，Python 旧数据不可自动迁移
- Python 版将继续保留于仓库，接收关键安全修复，但不增加新功能
- 新用户建议直接使用 Rust v1.2

> 📦 两个版本使用独立的 Git 仓库管理历史和远程地址。

---

## 路线图

以下功能已在架构中预留接口，进入 **延后（deferred）** 队列，不在 v1.2 发布范围内：

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
- 底层动机标注（schema 已在 v1.2 预埋 `memory_events.motives`，v1.3+ 激活）
- 取消生成、编辑历史发言、重新生成、分支对话
- tiktoken-rs 精确 token 化
- Character Card JSON/PNG 导出

> 如有需求，请在 [GitHub Issues](https://github.com/entergirl/Ramaria-s/issues) 提出。

---

## 许可证

[MIT](LICENSE) © 2026 黎烧酒
