# Ramaria Rust 重构软件开发计划书

> 版本：v4.0  
> 日期：2026-05-30  
> 决策依据：`docs/rust重构决策列表.md`  
> 基于：Ramaria v0.7.0 Python 源码分析  
> 目标：Rust 重写，Tauri 桌面应用 + CLI 终端  

---

## 0. 决策摘要

Rust 版是 Ramaria 的下一代实现。v1.0 发布后 Python 版进入维护模式，不再双线演进。当前确认无存量用户，因此 Rust v1.0 不兼容旧 Python 数据库，也不提供旧库自动迁移。

Rust 项目放在当前仓库的 `rust/` 子目录中。旧 Python 代码只读参考，不移动、不修改。

v1.0 聚焦核心闭环：

- Tauri 桌面应用，Windows 首发。
- CLI，作为脚本、调试和自动化入口。
- 记忆系统 L0/L1/L2/L3。
- 混合 RAG：向量检索、BM25、知识图谱通道。
- 本地 LLM：LM Studio。
- 线上 LLM：DeepSeek、OpenAI。
- 首次启动配置向导。
- OS keychain 存储线上 API key。
- 系统托盘和桌面通知。
- JSON/Markdown 导出。

v1.0 明确不做：

- Python 旧数据库兼容与自动迁移。
- FastAPI HTTP 服务、浏览器模式。
- Ollama、Claude、通义千问等额外后端。
- QQ/微信/Telegram/Discord/Slack 导入器。
- MCP/Telegram bridge。
- 自动更新。
- ratatui TUI。
- 运行时插件。
- NER 或 LLM 驱动复杂图谱抽取。
- 取消生成、编辑历史发言、重新生成、分支对话。
- 本地数据库加密、portable mode。

---

## 1. 项目背景

Ramaria（珊瑚菌）是一个本地运行的个人 AI 陪伴记忆系统。核心能力是分层记忆体系：不仅记录对话，还持续沉淀用户经历、偏好、关注点和长期画像。

当前 Python v0.7.0 的主要问题：

| 问题 | 影响 |
|---|---|
| Python 运行时、Chroma、embedding 模型加载导致启动慢 | 桌面体验不稳定 |
| 后台驻留内存较高 | 长期运行成本高 |
| PyInstaller 包体大 | 分发体验差 |
| 全局锁和运行时类型错误 | 并发和可靠性不足 |
| 测试体系不完整 | 回归不可控 |
| FastAPI/浏览器模式与桌面形态割裂 | 架构复杂度高 |

Rust 重构目标：

| 维度 | Rust v1.0 目标 |
|---|---|
| 启动体验 | GUI 冷启动到窗口可见 <= 1s，不含模型下载 |
| 空闲内存 | 启动后静置 10s RSS <= 200MB |
| 包体 | CLI binary 约 8MB，desktop installer 约 25MB |
| 架构 | Tauri IPC 替代 HTTP API |
| 安全 | API key 使用 OS keychain，线上调用需 provider 级隐私确认 |
| 测试 | cargo test / nextest / clippy / fmt / llvm-cov |

---

## 2. 产品形态

```text
Ramaria v1.0
├── Desktop App (Tauri 2)
│   ├── WebView 对话界面
│   ├── 首次配置向导
│   ├── LM Studio / DeepSeek / OpenAI 后端选择
│   ├── embedding 模型选择与下载
│   ├── 系统托盘
│   ├── 桌面通知
│   └── 记忆查看与基础设置
│
└── CLI
    ├── ramaria ask
    ├── ramaria chat
    ├── ramaria memory
    ├── ramaria session
    ├── ramaria config
    ├── ramaria index
    └── ramaria export
```

桌面应用是普通用户主入口。CLI 是脚本、调试、自动化和无 GUI 环境的入口。

---

## 3. 首次启动与配置向导

未完成首次配置前，GUI/CLI 不进入对话，只进入配置向导。

配置完成条件：

- 数据目录可写。
- SQLite migration 成功。
- 向量索引目录可写。
- 选择并验证一个 LLM 后端：LM Studio、DeepSeek 或 OpenAI。
- 如果选择 DeepSeek/OpenAI，API key 已成功写入 OS keychain。
- 选择 embedding 模型。
- embedding 模型下载完成。
- embedding 模型能成功生成一条测试向量。

未选择或未成功安装 embedding 模型前，应用不可正常使用。不提供 BM25-only 降级模式。

embedding 模型选择和下载在 GUI 系统设置界面、CLI 首次启动配置向导中完成，不阻塞应用窗口出现，但阻止进入正常使用状态。

应用状态机：

| 状态 | 含义 |
|---|---|
| `NeedsSetup` | 首次配置未完成 |
| `DownloadingModel` | embedding 模型下载中 |
| `Indexing` | 索引初始化或重建中 |
| `Ready` | 可正常对话 |
| `Degraded` | LLM 暂不可用等可恢复故障 |
| `FatalError` | 数据库、keychain、配置等不可恢复错误 |

---

## 4. 技术架构

### 4.1 仓库布局

```text
F:\Ramaria\Ramaria v0.x
├── app/                         # Python 旧代码，只读参考
├── src/                         # Python 旧代码，只读参考
├── static/                      # Python 旧前端，只读参考
├── docs/
│   ├── rust-rewrite-analysis.md
│   ├── rust重构决策列表.md
│   └── rust-migration-map.md
└── rust/
    ├── Cargo.toml
    ├── crates/
    │   ├── ramaria-core/
    │   ├── ramaria-storage/
    │   ├── ramaria-memory/
    │   ├── ramaria-llm/
    │   ├── ramaria-app/
    │   ├── ramaria-cli/
    │   └── ramaria-desktop/
    └── plugins/
        ├── llm/
        ├── embedding/
        ├── push/
        └── export/
```

Agent 工作边界：

- 允许修改 `rust/`。
- 允许修改 Rust 重构相关 `docs/`。
- 不修改旧 Python 代码。
- 不移动 Python 目录。

### 4.2 Crate 边界

依赖方向：

```text
ramaria-cli / ramaria-desktop
          ↓
     ramaria-app
          ↓
ramaria-memory / ramaria-llm
          ↓
   ramaria-storage
          ↓
     ramaria-core
```

| Crate | 职责 | 禁止事项 |
|---|---|---|
| `ramaria-core` | 类型、trait、错误、配置类型、注册抽象 | 不依赖数据库、网络、tokio |
| `ramaria-storage` | SQLite、索引存取、检索索引 | 不写业务聚合 |
| `ramaria-memory` | L1/L2/L3、衰减、图谱、冲突检测 | 不依赖具体 LLM provider |
| `ramaria-llm` | provider 抽象、OpenAI-compatible client、DeepSeek/OpenAI/LM Studio 后端 | 不写 UI 编排 |
| `ramaria-app` | 应用用例编排，供 CLI/Desktop 共用 | 不直接处理 UI 展示 |
| `ramaria-cli` | clap、REPL、命令输出 | 不写业务逻辑 |
| `ramaria-desktop` | Tauri Commands、Event、托盘、通知 | 不写业务逻辑 |

插件系统 v1.0 只做编译期 feature + trait crate，不做运行时动态加载。

---

## 5. 数据设计

### 5.1 基础原则

- SQLite 是事实源。
- 时间字段统一使用 Unix 毫秒时间戳，SQLite `INTEGER`。
- ID 使用 UUID v4，SQLite `TEXT`。
- 向量索引可重建，不作为事实源。
- Python 版和 Rust 版不允许交替写同一数据库。

### 5.2 数据目录

| 场景 | 路径 |
|---|---|
| Windows 默认 | `%APPDATA%\Ramaria\data\assistant.db` |
| 开发模式 | `rust/.ramaria-dev/assistant.db` |
| 覆盖 | `RAMARIA_DATA_DIR` |

配置文件：

- 用户配置：`%APPDATA%\Ramaria\config.toml`
- 开发覆盖：`.env`
- 环境变量优先级最高。
- API key 不写入 config，使用 OS keychain。

### 5.3 核心表

建议首版 schema：

| 表 | 用途 |
|---|---|
| `schema_meta` | schema version、index version |
| `sessions` | 会话生命周期 |
| `messages` | L0 原始消息 |
| `memory_l1` | L1 会话摘要 |
| `memory_l2` | L2 聚合摘要 |
| `l2_sources` | L2 到 L1 溯源 |
| `user_profile` | L3 用户画像 |
| `keyword_pool` | 关键词池 |
| `bm25_index` | BM25 token 持久化 |
| `graph_nodes` | 图谱节点 |
| `graph_edges` | 图谱边 |
| `privacy_consent` | provider/base_url 隐私确认 |
| `backend_config` | 非敏感后端配置 |
| `background_jobs` | 后台任务状态 |

### 5.4 索引一致性

- 每条可检索 memory 记录保存 `indexed_at` 和 `index_version`。
- 启动时若发现索引版本不一致，进入 `Indexing`。
- 索引重建完成前不进入正常对话。
- 索引重建失败可重复执行。

---

## 6. 记忆系统

分层记忆保持现有产品核心：

```text
L0 原始消息
  ↓ session 结束
L1 单次会话摘要
  ↓ 多条 L1 聚合
L2 时间段聚合摘要
  ↓ 多条 L2 提炼
L3 用户画像
```

原则：

- Session 边界、L1/L2/L3 生成时机、后台任务机制优先按现有 Python 版 session 设置和记忆逻辑迁移。
- 若迁移中发现旧逻辑不完整，再形成单独 ADR。
- 后台任务和索引状态必须可观测、可重试。

`ramaria-memory` 只依赖 LLM trait，便于注入 mock 后端做单元测试。

---

## 7. 混合 RAG

v1.0 必须实现三通道：

```text
用户消息
├── 向量检索
├── BM25
└── 知识图谱
       ↓
RRF 融合
       ↓
Ebbinghaus 衰减/加权
       ↓
System Prompt 注入
```

### 7.1 向量检索

- 用户首次配置时选择并下载 embedding 模型。
- 模型未完成配置前不可正常使用。
- 首选 Qdrant Edge。
- Phase 0 必须验证 Qdrant Edge：持久化正确性、检索延迟、API 稳定性。
- POC 不通过时切换 usearch 或其他本地 HNSW 方案。

### 7.2 BM25

- 使用 jieba-rs 分词。
- tokens 持久化到 SQLite，如 `bm25_index(doc_id, layer, tokens_json)`。
- 启动时加载入内存。
- 新增 memory 时增量更新。

### 7.3 图谱

- v1.0 不做 NER 或 LLM 驱动复杂图谱抽取。
- 优先沿用现有逻辑。
- 若需稳定简化实现，使用关键词共现图。

### 7.4 参数

RRF、Ebbinghaus 参数配置化，默认值沿用现有系统。RAG 效果验证指标延后单独讨论，不阻塞 Phase 0/Phase 1。

---

## 8. LLM 后端

### 8.1 支持范围

| Provider | 类型 | v1.0 状态 |
|---|---|---|
| LM Studio | 本地 | 必须支持 |
| DeepSeek | 线上 | 必须支持 |
| OpenAI | 线上 | 必须支持 |
| Ollama | 本地 | 延后 |
| Claude | 线上 | 延后 |
| 通义千问 | 线上 | 延后 |

### 8.2 LM Studio

- 默认 base URL：`http://localhost:1234/v1`
- 协议：OpenAI-compatible `/chat/completions`
- streaming 必须支持。
- 不要求用户手动指定或加载某个模型。
- 应用验证 LM Studio 正在运行、模型已下载、接口可用。
- 检测失败时提示用户启动 LM Studio 并完成模型下载。

### 8.3 DeepSeek/OpenAI

- API key 存入 OS keychain。
- provider 或 base_url 变化时重新确认隐私。
- base_url 可在高级设置中修改。
- 线上调用默认注入 L1/L2/L3 上下文，与本地一致。
- 配置中预留开关，可关闭线上记忆注入。

### 8.4 模型能力结构

预留字段：

- `provider`
- `model_id`
- `base_url`
- `supports_streaming`
- `supports_json_mode`
- `context_window`
- `max_output_tokens`

v1.0 不做复杂 token budget。轻度聊天场景下如上下文不足，按 RAG score 和记忆层级做简单截断。

---

## 9. 隐私与安全

### 9.1 隐私确认

- 线上 provider 单独确认。
- 记录 provider、base_url、timestamp、persistent。
- 用户可选择“下次不再提醒”。
- 勾选后跨重启持久化。
- 未勾选则重启后重新确认。
- 切回本地不撤销已持久化确认。

### 9.2 API Key

- 使用 OS keychain。
- service：`ramaria`
- account：`llm.deepseek.api_key`
- account：`llm.openai.api_key`
- 不 fallback 到明文文件。
- keychain 写入失败时，线上 provider 配置不可完成。

### 9.3 日志

- 默认目录：`%APPDATA%\Ramaria\logs\ramaria.log`
- 默认不记录完整用户消息、完整记忆、完整 prompt。
- 用户消息和记忆最多截断前 80 字符并附 SHA-256 哈希。
- `log_full_prompt = true` 默认关闭，需要显式开启并显示警告。
- 所有 LLM 调用、检索查询、索引任务、错误都带 `trace_id` 和耗时。

### 9.4 前端安全

- 配置严格 CSP。
- 禁止远程脚本。
- LLM 输出支持 Markdown，但禁用原始 HTML。
- 渲染前 sanitize。
- Tauri capabilities 最小化：dialog、notification、fs 数据目录、store 非敏感配置。

---

## 10. CLI 设计

命令名保持 `ramaria`。

v1.0 子命令：

```text
ramaria ask
ramaria chat
ramaria memory
ramaria session
ramaria config
ramaria index
ramaria export
```

行为：

- `chat` 做普通 REPL，不做 ratatui。
- `ask` 支持 `--json`。
- `ask` 和 `chat` 默认流式输出，`--no-stream` 关闭。
- CLI 与 GUI 共享配置和数据目录。
- `ramaria index rebuild` 用于重建索引。
- `ramaria export` 导出 JSON/Markdown。
- 线上调用交互式确认；脚本可用 `--yes` 跳过，但必须显式指定 provider。

---

## 11. Tauri 桌面设计

Tauri 是 v1.0 主交付物。

### 11.1 前端迁移

- 基本保留现有视觉设计。
- CSS 变量、配色、布局选择性复用。
- 通信层集中改造 `api.js`。
- `fetch` 改为 `invoke`。
- WebSocket 改为 Tauri Event。
- 不支持浏览器模式。

### 11.2 Commands

Tauri Commands 覆盖核心功能：

- 发送消息。
- 流式接收回复。
- 查询/切换后端。
- 首次配置。
- embedding 模型下载状态。
- 记忆查看。
- 会话列表。
- 配置读写。
- 索引重建。
- 导出。

Commands 只调用 `ramaria-app`。

### 11.3 流式事件

使用 Tauri Event，事件至少包含：

- `request_id`
- `delta`
- `done`
- `error`
- `backend_id`
- `created_at`

v1.0 不做取消生成。

### 11.4 系统集成

- 系统托盘是 v1.0 必须项。
- 桌面通知是 v1.0 必须项。
- 自动更新延后到 v1.1。

---

## 12. 错误类型

`ramaria-core` 定义稳定错误分类：

- `Config`
- `Storage`
- `Llm`
- `Privacy`
- `Index`
- `Validation`
- `Io`
- `Unsupported`

CLI 和 Desktop 根据错误类型展示不同提示。

---

## 13. 开发阶段

### Phase 0：风险 POC（1 周）

目标：先验证不可逆技术风险，避免直接全面铺代码。

任务：

- 建立 `rust/` workspace skeleton。
- 建立全部 crate skeleton。
- 建立 CI。
- Tauri + static 前端最小壳。
- LM Studio OpenAI-compatible streaming POC。
- DeepSeek streaming POC。
- OpenAI streaming POC。
- OS keychain 存取 POC。
- Qdrant Edge POC。
- SQLite/sqlx migration POC。

验收：

- workspace 可编译。
- `cargo test` 通过。
- `cargo clippy -- -D warnings` 通过。
- `cargo fmt --check` 通过。
- LM Studio/DeepSeek/OpenAI 至少能完成最小 streaming demo。
- keychain 能写入、读取、删除测试 key。
- Qdrant Edge POC 通过或明确切换备用方案。

### Phase 1：Core + Storage（2 周）

任务：

- `ramaria-core` 类型、trait、错误、配置类型。
- `ramaria-storage` SQLite schema 与 migrations。
- sessions/messages CRUD。
- L1/L2/L3 基础 CRUD。
- privacy_consent/backend_config/background_jobs 表。
- BM25 token 持久化结构。
- 索引版本元信息。

验收：

- storage 单元测试覆盖率 >= 90%。
- migration 可重复创建空库。
- CRUD 测试覆盖成功、失败、边界情况。

### Phase 2：Memory + Retrieval（3 周）

任务：

- L0/L1/L2/L3 生命周期迁移。
- Ebbinghaus 衰减。
- RRF 融合。
- BM25 检索。
- 向量索引接入。
- 图谱通道基础版。
- 后台任务状态与重试。
- 索引一致性检测和重建。

验收：

- memory 单元测试覆盖率 >= 70%。
- mock LLM 下可完成 L0 -> L1 -> L2 -> L3 流程。
- `ramaria index rebuild` 可重建索引。
- embedding 未配置时不可进入 Ready。

### Phase 3：LLM + App 编排（2 周）

任务：

- OpenAI-compatible client。
- LM Studio backend。
- DeepSeek backend。
- OpenAI backend。
- provider/base_url 隐私确认。
- keychain API。
- System Prompt 构建。
- `ramaria-app` 对话编排。

验收：

- mock backend 测试覆盖 app 编排。
- LM Studio/DeepSeek/OpenAI 手动 smoke test 通过。
- 线上 provider 隐私确认逻辑正确。
- API key 不落入 config/log。

### Phase 4：CLI（1 周）

任务：

- clap 子命令。
- 首次配置向导。
- `ask` 流式输出。
- `chat` REPL。
- `memory/session/config/index/export`。
- `--json`、`--no-stream`、`--yes`。

验收：

- CLI 可完成首次配置。
- CLI 可完成一轮对话并写入 L0。
- CLI 可触发索引重建和导出。

### Phase 5：Desktop（3 周）

任务：

- Tauri 项目。
- 前端迁移与 `api.js` 改造。
- 首次配置界面。
- 模型下载与状态展示。
- 对话界面。
- 记忆查看。
- 设置页。
- 系统托盘。
- 桌面通知。
- CSP 与安全渲染。

验收：

- Windows 桌面应用可完成首次配置。
- 可通过 LM Studio/DeepSeek/OpenAI 对话。
- 记忆写入、检索、展示可用。
- 系统托盘和通知可用。

### Phase 6：打包、测试、文档（2 周）

任务：

- Windows installer。
- README 更新。
- 快速开始。
- CLI 文档。
- 隐私说明。
- 开发者架构文档。
- `docs/rust-migration-map.md` 完成。

验收：

- Windows 安装包可安装运行。
- CI 三平台编译和单元测试通过。
- 文档覆盖安装、首次配置、后端配置、隐私说明。

---

## 14. 里程碑

| 里程碑 | 时间 | 可交付物 |
|---|---|---|
| M0 | 第 1 周末 | Phase 0 POC 通过，核心技术路线锁定 |
| M1 | 第 3 周末 | core/storage 可用，SQLite schema 稳定 |
| M2 | 第 6 周末 | memory/retrieval 闭环，索引可重建 |
| M3 | 第 8 周末 | LLM/app 编排完成，三后端 smoke test 通过 |
| M4 | 第 9 周末 | CLI 可用 |
| M5 | 第 12 周末 | Tauri 桌面可用 |
| M6 | 第 14 周末 | Windows 安装包、文档、CI 完成 |

---

## 15. 测试策略

工具链：

- `cargo test`
- `cargo nextest`
- `cargo clippy -- -D warnings`
- `cargo fmt --check`
- `cargo llvm-cov`

覆盖率目标：

| 模块 | 行覆盖率目标 |
|---|---|
| storage | >= 90% |
| memory | >= 70% |
| cli/desktop | >= 60% |

CI 范围：

- 不跑真实 LLM。
- 不下载 embedding 大模型。
- 不跑 Tauri 打包。
- 使用 mock LLM 和 mock embedding。
- Day 1 配置 Windows/macOS/Linux 编译和单元测试。

测试数据：

- 固定中文对话 fixture。
- LLM JSON 输出使用 schema validation。
- RAG 效果验证指标延后单独 ADR。

---

## 16. 性能指标

| 指标 | 目标 | 说明 |
|---|---|---|
| GUI 冷启动 | <= 1s 到窗口可见 | 不含模型下载、LM Studio 检测 |
| 空闲 RSS | <= 200MB | 启动后静置 10s |
| 对话峰值 RSS | <= 400MB | 连续对话场景 |
| 索引重建峰值 RSS | <= 600MB | 大数据重建场景 |
| CLI binary | 约 8MB | 不含模型 |
| Desktop installer | 约 25MB | 不含用户下载模型 |
| Installed size | 约 80MB | 不含用户下载模型 |

基准机器：

- Windows 11
- 16GB RAM
- 4 核 CPU
- SSD

数据规模：

- 1k messages：CI 快速验证。
- 10k messages：日常基准。
- 100k messages：压力测试。

---

## 17. 风险管理

| 风险 | 概率 | 影响 | 缓解措施 |
|---|---:|---:|---|
| Qdrant Edge API 或持久化不稳定 | 中 | 高 | Phase 0 POC；失败切 usearch/本地 HNSW |
| OS keychain 跨平台行为差异 | 中 | 中 | Phase 0 POC；封装 trait；CI 覆盖三平台编译 |
| LM Studio streaming 兼容差异 | 中 | 中 | 按 OpenAI-compatible 协议封装；提供连接诊断 |
| embedding 模型下载失败 | 中 | 中 | 配置向导停留并可重试；不进入 Ready |
| Tauri 前端迁移成本高 | 中 | 中 | 仅重写 `api.js` 通信层，保留 UI 结构 |
| LLM JSON 输出不稳定 | 高 | 中 | schema validation、重试、fallback parser |
| 三通道 RAG 首版复杂 | 中 | 高 | 先保证接口和基础实现，效果指标后续 ADR |
| 14 周计划仍偏紧 | 中 | 中 | Phase 0 后复盘，必要时收缩桌面次要功能 |

---

## 18. Python 到 Rust 映射

初始映射：

| Python 模块 | Rust 目标 |
|---|---|
| `src/ramaria/config.py` | `ramaria-core/src/config.rs` |
| `src/ramaria/exceptions.py` | `ramaria-core/src/error.rs` |
| `src/ramaria/logger.py` | tracing 初始化，位于 app/cli/desktop |
| `src/ramaria/storage/database.py` | `ramaria-storage` |
| `src/ramaria/storage/vector_store.py` | `ramaria-storage` retriever/vector/bm25 |
| `src/ramaria/memory/*` | `ramaria-memory` |
| `src/ramaria/core/llm_client.py` | `ramaria-llm` |
| `src/ramaria/core/prompt_builder.py` | `ramaria-llm` 或 `ramaria-app` prompt 模块 |
| `src/ramaria/core/router.py` | `ramaria-app` + `ramaria-llm` |
| `src/ramaria/core/session_manager.py` | `ramaria-app` + `ramaria-storage` |
| `app/routes/*` | Tauri Commands |
| `static/js/api.js` | Tauri invoke/Event 适配层 |
| `app/system/tray.py` | `ramaria-desktop` tray |

详细状态追踪放入 `docs/rust-migration-map.md`。

---

## 18. Git 仓库管理

采用**嵌套 Git 仓库**策略：Rust 代码在 Python 项目根目录 `rust/` 下开发，享受同目录参考 Python 源码的便利，同时拥有独立的 Git 历史和仓库。

```
f:\Ramaria\Ramaria v0.x\           # Python 仓库（main，进入维护模式）
├── .git/                           # 外层 Git — 追踪 Python 代码
├── .gitignore                      # 含 rust/（排除内层仓库）
├── src/ramaria/                    # Python 源码（只读参考）
├── docs/                           # 共享文档（决策列表、计划书等）
└── rust/                           # ⬅ Rust 仓库（独立 git，嵌套在外层仓库下）
    ├── .git/                       # 内层 Git — 完全独立
    ├── Cargo.toml                  # workspace 根
    ├── crates/
    └── docs/
```

**操作流程：**

```bash
# 初始化（一次性）
cd f:\Ramaria\Ramaria v0.x\rust
git init
git remote add origin https://github.com/yourname/ramaria-rs.git

# 日常工作流
# Step 1: 在 rust/ 下正常开发
cd rust
# ... 写代码、cargo test ...

# Step 2: 提交 Rust 代码
git add -A
git commit -m "feat(storage): add SQLite schema migration"
git push origin main

# Step 3: 提交文档（如决策列表、计划书有更新）
cd ..
git add rust/docs/
git commit -m "docs: update decision list §5 LLM privacy"
git push origin main
```

> **注意**：`rust/docs/` 目录下的文档在两个仓库中都会被追踪，更新时记得两边同步。

**与独立仓库方案对比：**

| 维度 | 嵌套 Git | 独立文件夹 + 手动复制 |
|------|---------|---------------------|
| Python 源码参考 | ✅ 同根目录直接打开 | ❌ 需切换文件夹 |
| 提交 Rust 代码 | 在 `rust/` 内 `git commit` | 复制到独立仓库再 commit |
| 遗漏风险 | 零（直接在工作目录提交） | 有（忘记复制） |
| Git 隔离性 | ✅ 内外层 Git 完全隔离 | ✅ 两个独立文件夹 |

---

## 19. 延后事项

以下事项不阻塞 v1.0 核心闭环：

- RAG 效果验证指标和人工标注集。
- embedding 模型候选列表。
- 图谱抽取升级方案。
- 运行时插件机制。
- macOS/Linux 安装包。
- 自动更新。
- SQLCipher。
- portable mode。
- 更细粒度的线上记忆注入控制。
- 取消生成、编辑历史发言、重新生成、分支对话。

---

## 20. 交付判定

Rust v1.0 可以发布的最低标准：

- Windows 安装包可安装运行。
- 首次配置向导可完成。
- LM Studio、DeepSeek、OpenAI 至少各通过一次手动 smoke test。
- embedding 模型完成下载并能生成测试向量。
- GUI 可完成对话、保存消息、生成记忆、检索记忆。
- CLI 可完成 `ask`、`chat`、`index rebuild`、`export`。
- 系统托盘和通知可用。
- API key 不落入配置文件或日志。
- CI 三平台编译和测试通过。
- `cargo clippy -- -D warnings` 和 `cargo fmt --check` 通过。

