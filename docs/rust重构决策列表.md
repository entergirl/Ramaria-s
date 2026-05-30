# Ramaria Rust 重构决策最终版

> 状态：已对齐  
> 日期：2026-05-30  
> 适用范围：Rust 重构计划书、Agent 执行约束、Phase 0/Phase 1 开发任务  

本文是 Rust 重构的单一决策源。后续计划书、任务拆分和 agent 指令均以本文为准。

---

## 0. 总体结论

Rust 版定位为 Ramaria 的下一代实现。v1.0 发布后 Python 版进入维护模式，不再并行演进。由于当前确认无存量用户，Rust v1.0 不兼容旧 Python 数据库，也不提供旧库自动迁移。

Rust 项目放在当前仓库的 `rust/` 子目录中。旧 Python 代码只作为只读参考，不移动、不修改。开发计划书、决策文档和迁移映射文档可在 `docs/` 中维护。

v1.0 聚焦核心闭环：Tauri 桌面、CLI、记忆 L0-L3、混合 RAG、本地 LM Studio、线上 DeepSeek/OpenAI。MCP/Telegram/QQ/微信/Discord 导入、自动更新、ratatui TUI、运行时插件、复杂图谱抽取、portable mode、SQLCipher 延后。

---

## 1. 产品范围

### v1.0 必须交付

- Tauri 桌面应用，Windows 首发。
- CLI，作为脚本、调试和自动化入口。
- 记忆系统 L0/L1/L2/L3。
- 混合 RAG：向量检索、BM25、知识图谱通道。
- 本地 LLM：LM Studio。
- 线上 LLM：DeepSeek、OpenAI。
- 首次启动配置向导。
- OS keychain 存储线上 API key。
- 基础导出：JSON/Markdown。
- 系统托盘和桌面通知。

### v1.0 明确不做

- Python 旧数据库兼容与自动迁移。
- FastAPI HTTP 服务和浏览器模式。
- Ollama 后端。
- Claude、通义千问等额外线上后端。
- QQ/微信/Telegram/Discord/Slack 导入器。
- MCP/Telegram bridge。
- 自动更新。
- ratatui TUI，CLI `chat` 先做普通 REPL。
- 运行时插件。
- NER 或 LLM 驱动的复杂图谱抽取。
- 取消生成、编辑历史发言、重新生成、分支对话。
- 本地数据库加密、portable mode。

### 平台范围

- Windows：v1.0 首发，提供安装包。
- macOS/Linux：保证可编译和 CI 通过，安装包延后。

---

## 2. 仓库与 Agent 工作边界

Rust 项目路径：

```text
F:\Ramaria\Ramaria v0.x\rust
```

旧代码路径保持不变：

```text
F:\Ramaria\Ramaria v0.x\app
F:\Ramaria\Ramaria v0.x\src
F:\Ramaria\Ramaria v0.x\static
```

Agent 约束：

- 允许修改 `rust/`。
- 允许修改 `docs/` 中与 Rust 重构相关的文档。
- 不修改旧 Python 代码。
- 不移动 Python 目录。
- 参考 Python 逻辑时，按文件映射记录到 `docs/rust-migration-map.md`。

---

## 3. Rust Workspace 架构

Rust workspace 放在 `rust/` 子目录下。

建议 crate：

```text
rust/
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

依赖方向：

```text
cli/desktop -> app -> memory/llm -> storage -> core
```

边界规则：

- `ramaria-core`：零 I/O，不依赖数据库、网络、tokio；只放类型、trait、错误、配置类型、注册抽象。
- `ramaria-storage`：SQLite、索引存取、检索索引，不放业务聚合。
- `ramaria-memory`：L1/L2/L3、衰减、图谱、冲突检测；只依赖 LLM trait，不依赖具体 provider。
- `ramaria-llm`：provider 抽象、OpenAI-compatible client、DeepSeek/OpenAI/LM Studio 后端。
- `ramaria-app`：应用用例编排层，CLI 和 Desktop 都调用它。
- `ramaria-cli`：参数解析、REPL、命令输出，不写业务逻辑。
- `ramaria-desktop`：Tauri thin shell，Commands 只做入参/出参转换。

插件系统 v1.0 只做编译期 feature + trait crate，不做运行时动态加载。

---

## 4. 数据与存储

Rust v1.0 全新设计数据库，不兼容旧 Python schema。

基础决策：

- SQLite 是事实源。
- 时间字段统一使用 Unix 毫秒时间戳，SQLite `INTEGER`。
- ID 统一使用 UUID v4，SQLite `TEXT`。
- `messages.fingerprint` 使用 SHA-256 前 16 位 hex，用于同一原始数据重复导入去重。
- 旧 Chroma 数据不迁移。
- 向量索引可重建，不作为事实源。
- Python 版和 Rust 版不允许交替写同一个数据库。

数据目录：

- Windows 默认：`%APPDATA%\Ramaria\data\assistant.db`
- 开发模式：`rust/.ramaria-dev/assistant.db`
- CLI 和 GUI 共享同一数据目录。
- 支持 `RAMARIA_DATA_DIR` 覆盖。

配置目录：

- 用户配置：`%APPDATA%\Ramaria\config.toml`
- 开发覆盖：`.env`
- 环境变量优先级最高。
- API key 不写入 config，使用 OS keychain。

Schema 管理：

- 使用 `sqlx::migrate!`。
- 增加 `schema_version` 或等价元信息表。
- 索引版本由应用层管理。
- Schema migration 使用事务。
- 索引重建失败可重复执行。

索引一致性：

- 每条可检索 memory 记录保存 `indexed_at` 和 `index_version`。
- 启动时若发现索引版本不一致，进入重建索引状态。
- 索引重建完成前不进入正常对话。

---

## 5. 首次启动与配置向导

未完成首次配置前，GUI/CLI 不进入对话，只进入配置向导。

配置完成的最小条件：

- 数据目录可写。
- SQLite migration 成功。
- 向量索引目录可写。
- 选择并验证一个 LLM 后端：LM Studio、DeepSeek 或 OpenAI。
- 如果选择 DeepSeek/OpenAI，API key 已成功写入 OS keychain。
- 选择 embedding 模型。
- embedding 模型已下载完成。
- embedding 模型能成功生成一条测试向量。

未选择或未成功安装 embedding 模型前，应用不可正常使用。此阶段不提供 BM25-only 降级模式。

向量模型的选择和下载在 GUI 系统设置界面、CLI 首次启动配置向导中完成，不阻塞应用窗口出现，但阻止进入正常使用状态。

---

## 6. LLM 后端

v1.0 后端范围：

- 本地：LM Studio。
- 线上：DeepSeek、OpenAI。

LM Studio 约定：

- 默认 base URL：`http://localhost:1234/v1`
- 协议：OpenAI-compatible `/chat/completions`
- streaming：必须支持；不支持 streaming 的模型不可作为可用模型。
- 不要求用户手动指定或加载某个模型。
- 应用只验证：LM Studio 正在运行、模型已下载、接口可用。
- 检测失败时，GUI 提示用户启动 LM Studio 并完成模型下载。

DeepSeek/OpenAI 约定：

- API key 存入 OS keychain。
- DeepSeek 和 OpenAI 分别确认隐私。
- provider 或 base_url 变化时需要重新确认隐私。
- base_url 可在高级设置中修改。

模型能力结构预留字段：

- `provider`
- `model_id`
- `base_url`
- `supports_streaming`
- `supports_json_mode`
- `context_window`
- `max_output_tokens`

暂不处理复杂 token budget。v1.0 默认轻度聊天场景，上下文不足时按 RAG score 和记忆层级做简单截断，后续再引入 tokenizer 精算。

---

## 7. 隐私与密钥

隐私确认：

- 线上 provider 单独确认。
- 确认记录包含 provider、base_url、timestamp、persistent。
- 用户可选择“下次不再提醒”。
- 勾选后跨重启持久化。
- 未勾选则重启后重新确认。
- 切回本地不撤销已持久化确认。

线上记忆注入：

- v1.0 默认线上和本地一致，注入 L1/L2/L3 上下文。
- 配置中预留开关，可关闭线上记忆注入。
- GUI 粒度控制延后到 v1.1。

API key：

- 使用 OS keychain。
- 不 fallback 到明文文件。
- keychain 写入失败时，线上 provider 配置不可完成。

Keychain 命名：

- service：`ramaria`
- account：`llm.deepseek.api_key`
- account：`llm.openai.api_key`

日志隐私：

- 默认不记录完整用户消息、完整记忆、完整 prompt。
- 用户消息和记忆最多截断前 80 字符并附 SHA-256 哈希。
- `log_full_prompt = true` 默认关闭，需要显式开启并显示警告。

---

## 8. Embedding 与检索

Embedding 模型不在启动时自动选择。用户在首次配置向导中选择模型并完成下载。

向量存储：

- 首选 Qdrant Edge。
- Phase 0 必须做 POC 验证：持久化正确性、检索延迟、API 稳定性。
- POC 不通过时切换到 usearch 或其他本地 HNSW 方案。

检索通道：

- 向量检索。
- BM25。
- 知识图谱。

BM25：

- 使用 jieba-rs 分词。
- tokens 持久化到 SQLite，如 `bm25_index(doc_id, layer, tokens_json)`。
- 启动时加载入内存，增量更新。

图谱：

- 先按现有 session 设置和现有记忆逻辑迁移。
- v1.0 不做 NER 或 LLM 驱动复杂图谱抽取。
- 若需要稳定实现，优先使用关键词共现图。

RRF/Ebbinghaus：

- 参数配置化。
- 默认值沿用现有系统。
- 效果验证指标延后单独讨论。

---

## 9. Session 与记忆生命周期

Session 边界、L1/L2/L3 生成时机、后台任务机制、索引一致性策略，优先按现有 Python 版 session 设置和记忆逻辑迁移。

原则：

- 先保持现有行为，不在 Rust v1.0 中重新设计 session 策略。
- 如迁移过程中发现旧逻辑不完整，再形成单独 ADR。
- 后台任务和索引状态应可观测、可重试。

---

## 10. CLI

命令名保持 `ramaria`。

v1.0 子命令：

- `ask`
- `chat`
- `memory`
- `session`
- `config`
- `index`
- `export`

CLI 行为：

- `chat` 做普通 REPL，不做 ratatui。
- `ask` 支持 `--json`。
- `ask` 和 `chat` 默认流式输出，`--no-stream` 关闭。
- CLI 与 GUI 共享配置和数据目录。
- `ramaria index rebuild` 用于重建索引。
- `ramaria export` 导出 JSON/Markdown。
- 线上调用交互式确认；脚本可用 `--yes` 跳过，但必须显式指定 provider。

最小安装：

- `ramaria-core` 不包含任何 LLM 后端。
- CLI 可无 GUI 独立安装。
- 用户必须至少启用一个 LLM feature 才能对话。

---

## 11. Tauri 桌面

Tauri 是 v1.0 主交付物。

前端策略：

- 基本保留现有视觉设计。
- CSS 变量、配色和布局选择性复用。
- 通信层集中改造 `api.js`。
- `fetch` 改为 `invoke`。
- WebSocket 改为 Tauri Event。
- 不再支持浏览器模式。

Tauri Commands：

- 覆盖原对话、记忆、配置、导入/导出等核心功能。
- Commands 只调用 `ramaria-app`。

流式输出：

- 使用 Tauri Event。
- 事件至少包含 `request_id`、`delta`、`done`、`error`、`backend_id`、`created_at`。
- v1.0 不做取消生成。

桌面功能：

- 系统托盘是 v1.0 必须项。
- 桌面通知是 v1.0 必须项。
- 自动更新延后到 v1.1。

安全：

- 配置严格 CSP。
- 禁止远程脚本。
- 用户消息和 LLM 输出按文本/Markdown 安全渲染。
- 禁用原始 HTML，渲染前 sanitize。
- Tauri capabilities 最小化：dialog、notification、fs 数据目录、store 非敏感配置。

---

## 12. 状态机

桌面和 CLI 共享应用状态：

- `NeedsSetup`：首次配置未完成。
- `DownloadingModel`：embedding 模型下载中。
- `Indexing`：索引初始化或重建中。
- `Ready`：可正常对话。
- `Degraded`：LLM 暂不可用等可恢复故障。
- `FatalError`：数据库、keychain、配置等不可恢复错误。

缺少 embedding 模型不进入 `Degraded`，而是停留在 `NeedsSetup` 或 `DownloadingModel`。

---

## 13. 错误与日志

`ramaria-core` 定义稳定错误分类：

- `Config`
- `Storage`
- `Llm`
- `Privacy`
- `Index`
- `Validation`
- `Io`
- `Unsupported`

UI 和 CLI 根据错误类型展示不同提示。

日志：

- 默认目录：`%APPDATA%\Ramaria\logs\ramaria.log`
- 所有 LLM 调用、检索查询、索引任务、错误都带 `trace_id` 和耗时。
- ERROR 级别包含错误链和必要上下文。
- 不默认记录完整 prompt 或用户内容。

---

## 14. 测试与验收

Rust 测试工具链：

- `cargo test`
- `cargo nextest`
- `cargo clippy -- -D warnings`
- `cargo fmt --check`
- `cargo llvm-cov`

覆盖率目标：

- storage >= 90%
- memory >= 70%
- cli/desktop >= 60%
- 指标为行覆盖率。

测试原则：

- CI 不跑真实 LLM。
- CI 不下载 embedding 大模型。
- CI 不跑 Tauri 打包。
- 使用 mock LLM 和 mock embedding。
- 固定中文对话 fixture 用于检索和记忆测试。
- LLM JSON 输出使用 schema validation。

RAG 效果验证延后单独讨论，不作为最早期阻塞项。

---

## 15. 性能与指标

启动时间：

- GUI 冷启动：从双击 exe 到窗口可见。
- 不包含 embedding 模型下载。
- 不包含 LM Studio 连接检测。
- 模型加载和外部服务检测异步进行。

内存：

- 空闲驻留目标：启动后静置 10s 的 RSS <= 200MB。
- 对话峰值可放宽到 400MB。
- 索引重建峰值可放宽到 600MB。

包体：

- CLI binary 目标约 8MB。
- Desktop installer 目标约 25MB。
- Installed size 目标约 80MB。
- 不含用户下载的 embedding 模型。

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

## 16. 安全与隐私

本地数据加密：

- v1.0 不做。
- v1.1+ 考虑 SQLCipher。

本地数据管理：

- 支持清除全部记忆。
- 支持按 session 删除。
- 支持导出全部记忆。
- `data_dir` 可配置，默认 `%APPDATA%/Ramaria`。
- portable mode 延后。

Crash report：

- 本地化保存。
- 不自动上传。
- 用户可手动发送。

插件网络权限：

- v1.0 编译期插件无运行时权限模型。
- 只有线上 LLM 后端需要网络。
- MCP/Telegram 等桥接默认不编译。

---

## 17. 开发流程

第一步：

- 建立 `rust/` workspace skeleton。
- 建立全部 crate skeleton。
- 建立 CI。
- 保证空骨架可编译。

开发顺序：

```text
Phase 0: 风险 POC
Phase 1: core + storage
Phase 2: memory + retrieval
Phase 3: llm + app
Phase 4: cli
Phase 5: desktop
Phase 6: package + docs
```

Phase 0 必须验证：

- Tauri + static 前端最小壳。
- LM Studio OpenAI-compatible streaming POC。
- DeepSeek streaming POC。
- OpenAI streaming POC。
- OS keychain 存取 POC。
- Qdrant Edge POC。
- SQLite/sqlx migration POC。

提交标准：

- 每迁移一个模块必须有测试。
- CI 阻塞条件：`cargo test`、`cargo clippy -- -D warnings`、`cargo fmt --check`。
- 大模块独立提交，小模块可合并。

仓库策略：

- Rust 项目使用嵌套 Git 仓库，位于 Python 仓库根目录 `rust/`。
- 内层 `rust/.git` 与外层 Git 完全隔离，独立历史、独立远程。
- 外层 `.gitignore` 已排除 `rust/`，不会交错推送。
- `rust/docs/` 下的计划书和决策列表在两仓库间保持同步。

文档：

- `docs/rust重构决策列表.md` 是决策 SSOT。
- 新增 `docs/rust-migration-map.md` 映射 Python 文件到 Rust crate。
- 任务编号格式：`T-STORAGE-001`。

版本：

- Rust 开发初期版本从 `0.1.0` 开始。
- 首个正式替代 Python 的发布版本为 `1.0.0`。
- Python 版打 `v0.7.x-final` tag 后进入维护模式。

MSRV：

- Rust 1.80+。

规范：

- `cargo fmt` 标准风格。
- `cargo clippy -- -D warnings` 禁止警告。
- Conventional Commits：`feat:`、`fix:`、`chore:` 等。

---

## 18. 延后决策

以下事项不阻塞早期开发，后续单独 ADR：

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

