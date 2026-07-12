# 变更日志

本文档记录 Ramaria Rust 版的所有显著变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

---

## [1.2.0] - 2026-07-07

### 核心特性

#### Pipeline + Stage 架构重构（🔴 P0）

- 将 `send_message` 10 步单体方法重构为 Pipeline + Stage 模式
- `PipelineStage` trait：统一接口，关联类型 `Input`/`Output`，`async execute()`
- `PipelineContext`：全 `Arc` 引用共享上下文，零拷贝传递（storage/llm/embedding/config/retriever/keychain/lifecycle）
- `PipelineData`：数据载体，承载 10 个 Stage 的中间结果
- `PipelineError`：区分 `Retryable`/`Fatal`，编排器在第一处 Fatal 错误时中止
- `SendMessagePipeline`：编排器，按顺序执行 Stage 序列
- 10 个独立 Stage，各自可注入 mock 依赖编写确定性单元测试
- 新增 ≥ 60 个单元测试 + 集成测试覆盖全流程正常路径和错误传播

#### Session-Persona 绑定（🔴 P0）

- `sessions` 表新增 `persona_uid TEXT` 列（增量 migration，`DEFAULT NULL` 兼容存量）
- `create_session` 签名新增 `persona_uid` 参数，创建时写入当前对话人格
- 用户消息 `persona_uid` 统一填入当前对话人格（不再为 NULL）
- Persona 切换由后端主导：优先从 `session.persona_uid` 读取，NULL 时回退前端传参
- 前端 `personaSessions` 降级为性能缓存
- 导入历史会话绑定正确人格（`create_historical` 新增 `persona_uid` 参数）

#### L3 管线贯通（🔴 P0）

- 新建 `ramaria-memory/src/inference/orchestrator.rs`：`run_phase_b_inference` + `run_phase_c_update`
- Phase B 三步 LLM 结构化推断完整接通（逐分类信号→跨分类一致性→合成三层画像）
- JSON 三步解析 + 五档钳制降级策略；LLM 全失败时回退 `mock_infer` 产出 `TraitSource::Statistical`
- Phase C 置信度更新 + Wasserstein 漂移检测 + 证据链记录
- Phase B/C 写入 `personality_traits` + `trait_evidence`；完成后标记事件已吸收
- `run_l3_inference` 全流程（Phase A→B→C）在 mock LLM 下跑通
- 新增 `InferenceConfig` 配置项（含 4 个子配置，合理默认值）
- 新增 ≥ 37 个测试（30 个纯函数 + 7 个端到端集成测试）

#### 前端记忆与对话联动（🟡 P1）

- **SessionDrawer 组件**：对话页左侧会话历史抽屉，点击 Header "📋 历史"按钮滑出
  - 180ms slide 动画、搜索过滤、活跃/已关闭/导入标签区分
  - 点击会话项加载消息，已关闭会话自动只读
  - 加载骨架屏 + 错误重试 + ESC/外部点击关闭
- **L1 记忆卡片跳转**：卡片底部"💬 查看对话 (N 条消息)"按钮
  - `Router.showView` 扩展 `options`（`sessionId`/`personaUid`/`fromView`）
  - ChatView 顶部"← 返回记忆"面包屑，记忆页恢复之前状态
- **L1 卡片 UI 重新设计**：
  - valence 情感色条（正面=粉渐变/负面=蓝渐变/中性=灰），顶部 3px
  - 属性行并排展示（时段 + 氛围 + 参与人数）
  - 关键词 chip 标签替代逗号分隔文本
  - 底部操作栏（时间 + 强度条 + "💬 查看对话"按钮）
  - 旧卡片降级兼容（无 `context_json` 隐藏参与人数，无 `session_id` 隐藏跳转按钮）
- **导入进度 UI 增强**：进度条高度 ≥ 10px、阶段指示器"第 N/M 个会话"、预估剩余时间、暗色主题适配

#### 后端记忆持久化修复（🔴 P0）

- 空闲超时自动关闭和 shutdown 关闭路径的 `persona_uid` 不再丢失
  - 修复前：硬编码 `None`，L1 摘要归属 NULL → `list_recent_l1_by_persona` 查询不到
  - 修复后：从 active session 的 DB 记录读取 `persona_uid` 传入
- L1 摘要生成后立即增量更新 Retriever 内存索引
  - 新增 `Retriever::index_l1_record(&MemoryL1)` 公开方法
  - L1 生成后立即可通过 Stage 5 RAG 检索命中，不需等待手动 rebuild
  - BM25 通道可即时命中（向量通道需 rebuild 路径生成）
- `App::new` 注入共享 `Arc<Mutex<Retriever>>` 到 `SessionLifecycle`
- 新增 9 个单元测试覆盖空闲/shutdown 路径 + Retriever 增量索引

### Schema 变更

- `sessions` 表新增 `persona_uid TEXT`（增量 migration，DEFAULT NULL）
- `memory_events` 表新增 `motives TEXT`（v1.3 激活，v1.2 仅预埋 schema，不修改业务逻辑）

### 工程改善

#### 测试

- 全 workspace 测试总数 ≥ 600（v1.1: 546，新增 ≥ 50 个）
- 新增 M1/M2/M3 集成测试文件（Mock 全依赖 Pipeline 流程 + L3 闭环验证）
- 新模块行覆盖率 ≥ 80%（`pipeline.rs`、`stages/`、`orchestrator.rs`）

#### 代码组织

- 新建 `ramaria-app/src/pipeline.rs`（~1320 行）+ `stages/` 目录（10 个 Stage 文件）
- 新建 `ramaria-memory/src/inference/orchestrator.rs`（~1400 行）
- `app_chat.rs` 逻辑拆分至各 Stage（`search_and_assemble_context`、`build_system_prompt_with_context` 等）
- `RunL3Inference` 中 `_llm` → `llm`，Phase B/C 调用链完整

#### 文档（v1.2）

- `chat-spec.md`：管线架构更新为 Pipeline + Stage 模式；Session-Persona 绑定；SessionDrawer；Retriever 增量索引
- `memory-spec.md`：L3 闭环 Phase A→B→C 全流程；orchestrator；L1 卡片跳转
- `arch-decisions-unified.md`：延后清单标注 L3 Phase B/C 在 v1.2 完成；`motives` 列已预埋
- README：版本号 v1.1.0 → v1.2.0；测试数量 ~600+

### Bug 修复

- 修复空闲超时/shutdown 关闭 session 时 `persona_uid` 丢失（L1 摘要归属 NULL）
- 修复 L1 生成后 Retriever 不更新的时序空隙（保存后立即检索命中）
- 修复导入历史会话 `persona_uid` 为 NULL（SessionDrawer 按 persona 筛选失效）
- 修复 SessionDrawer 竞态条件（`_isOpen` 过早设置导致 outside-click 处理器立即关闭抽屉）
- 修复对话界面空白（`chat.js` 多余 `*/` 导致 JS 语法错误）
- 修复 Embedding 查询失败（`llama_head_dim.rs` 未清除 KV cache）
- 修复保存对话后重进只显示旧会话（`personaSessions` 缓存未清除）

### 破坏性变更（开发者）

面向终端用户无破坏性变更。以下为内部 API 变更，不影响功能：

- `StorageBackend::create_session` 签名新增 `persona_uid: Option<&str>` 参数
- `create_historical` 签名新增 `persona_uid: &str` 参数
- `App.retriever` 类型从 `Mutex<Retriever>` 改为 `Arc<Mutex<Retriever>>`

### 已知限制

与 v1.1.0 相同的限制：
- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件
- 不支持 LLM 对话"重新生成"功能
- ONNX 模型需用户手动下载或配置

v1.2 新增：
- 存量 session（v1.1 及以前）`persona_uid` 为 NULL，在 SessionDrawer 中按 persona 筛选时归入默认人格。不影响正常对话，下次关闭 session 时自动填充。

---

## [1.1.0] - 2026-06-16

### 核心特性

#### Session 生命周期与记忆管线全自动触发

- 手动关闭：用户点击"保存对话"→ session 关闭 → L1 摘要 → 级联检查 L2/L3 触发。同一窗口继续对话，不清屏
- 空闲自动关闭：后台线程每 60s 轮询，空闲 > 10min 自动关闭 session 并触发记忆管线
- 只读约束：已关闭 session 禁止写入（DB 层拒绝 + 前端隐藏输入框），显示"此对话已关闭"提示
- shutdown hook：应用退出时自动关闭活跃 session，取消后台任务

#### 本地嵌入模型

- 集成 ONNX Runtime（`ort` v2.0-rc.12），运行 `bge-small-zh-v1.5`（384 维），feature gate `embedding-onnx`
- 模型下载管理：进度回调 + SHA-256 校验 + 断点续传
- BM25-only 降级模式：未配置嵌入模型时自动切到 `Degraded` 状态，RAG 仅用 BM25+图谱通道
- 对话页顶部进度条：下载/索引进度展示，5s 无事件自动隐藏
- RAG 检索适配 8 种通道组合（BM25/向量/图谱任意组合）

#### 情境强度加权 + Token Budgeting

- `memory_l1` / `memory_events` 新增 `situation_strength` 字段（1-5 级，默认 3）
- Phase A 统计推断加权：弱情境(1-2)×1.5、中性(3)×1.0、强情境(4-5)×0.5
- Token 预算分配：字符数估算(CJK≈len/2, 拉丁≈len/4) → System Prompt(1000) → RAG → History(新→旧)
- 句子边界优雅截断（`。！？\n`），不硬切

#### QQ 聊天记录导入器

- 新建 `ramaria-importer` crate（workspace 第 8 个 crate），compile-time feature gate
- 双格式支持：JSON（`qq-chat-exporter` v5.x）+ TXT（经典 PCQQ 导出）
- 多编码兼容：UTF-8 / UTF-8 BOM / UTF-16 LE / GBK
- 快速导入：仅写 `messages` 表 + `import_fingerprint` 去重
- 深度导入：历史 session → L0→L1→L2→L3 全管线
- 双画像自动创建：导出者和聊天对象各自独立 persona，UID 优先使用 QQ 号
- 角色前缀：`[烧酒] xxxx` / `[omkidaso] yyyy`，消除"用户 vs 助手"误导
- CLI: `ramaria import qq --file <PATH> [--deep] [--persona-self-name ...]`
- 桌面端：三步导入向导（文件选择→预览报告→确认导入）

#### 多角色管理 GUI

- Sidebar 新增 👥"人格"导航页，人格卡片网格展示
- 详情页在线编辑基本信息（名称/头像 URL/描述）
- 设为默认对话人格 / 重载性格按钮

#### 自动更新检查 + 诊断导出

- `check_update()`：GitHub Release API `/latest` + 语义版本号比较
- 设置页"诊断与更新"：版本号显示 + 检查更新按钮
- 诊断导出：日志(1000行) + config(脱敏) + schema_meta + OS 信息 → `.zip`
- CLI: `ramaria diagnostics --output <PATH>`

---

### 安全修复

- **CSP 收紧**：移除 `'unsafe-inline'`，行内脚本外部化到外部 JS 文件
- **errorText XSS**：`innerHTML` → `textContent`，防止 LLM 错误消息注入
- **路径穿越统一规范化**：CLI/Desktop 统一 `canonicalize()` + RootDir/Prefix 检查
- **窗口关闭超时恢复**：前端 N 秒未响应 → 自动回退 `hide()`，托盘始终可恢复
- **JobManager CancellationToken**：应用关闭时 `execute_with_retry` 优雅取消
- **job 状态标记失败终止**：不再静默继续执行
- **session list 真实查询 message_count**：SQL JOIN 替代硬编码
- **API Key 统一遮蔽**：前端显示 + 诊断导出统一 `[REDACTED]`

---

### 工程改善

#### 性能优化

- Retriever `l1_docs`/`l2_docs` 添加 LRU 淘汰（1500/1500 cap），防止内存无限增长
- storage 批量写入添加显式事务（`save_import_batch()`），减少 SQLite fsync 开销
- 前端 `_pendingDelta` 添加上限保护（超 10KB 强制刷新）
- BM25 `add()` 改为移动所有权 + `degrade` 使用 HashSet 去重 O(n)
- 模型下载 HTTP 客户端添加超时（30s connect + 3600s total）

#### 代码组织

- `app.rs` 大文件拆分：提取 `app_chat.rs`（644行）、`app_retriever.rs`（156行）、`app_state.rs`（209行）
- `app.rs` 从 1270→492 行（-61%）
- CLI `unsafe` 块补全 4 处 SAFETY 注释

#### 测试

- 总计 546 个测试函数（v1.0: ~530），覆盖全部 8 个 crate
- 新增集成测试 `tests/integration_tests.rs`（13 个跨 crate 测试）
- `ramaria-importer` 17 个单元测试 + 8 个双画像测试

#### CI

- 新增 `cargo llvm-cov` / `cargo deny` / `cargo audit` 三个非阻塞检查（仅报告）

#### 文档

- 桌面使用指南全文重写（新增人格管理/导入功能/诊断与更新/故障排除扩充）
- CLI 使用指南全文重写（新增 `ramaria import` / `ramaria diagnostics`）
- 隐私说明全文重写（新增导入数据/诊断脱敏/修正 CSP）
- 新建 `config/default.toml` 配置模板（9 节 130 行）
- README 数据库表清单修正（移除 5 个 ghost 表，补全 23 张表完整清单）

#### Schema 变更

- 3 个增量 migration（`situation_strength` / `event_situation` / `persona_description`），均可空、向后兼容
- 不创建新表，不修改既有列

---

### 已知限制

- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件，正式图标待设计师提供
- 不支持 LLM 对话"重新生成"功能
- ONNX 模型需用户手动下载或配置

---

## [1.0.1] - 2026-06-13

### 修复

#### 致命：全新安装后应用无法启动

- **插件配置反序列化错误**：`tauri.conf.json` 中 `plugins.dialog`、`plugins.notification`、`plugins.store` 使用了空对象 `{}`，Tauri v2 反序列化期望 `null`（unit 类型），导致应用在窗口创建前 panic 退出
- **影响**：所有不含开发依赖的干净 Windows 环境均受影响
- **修复**：三个插件配置值从 `{}` 改为 `null`
- **Schema URL**：`$schema` 从已失效的 `dev` 分支改为 `v2` 稳定分支

---

## [1.0.0] - 2026-06-12

### 核心特性

#### 分层记忆管线（L0→L1→L2→L3）

- L0 原始消息层：永久保留所有对话消息，标记发言人，按时间排序
- L1 单次摘要层：session 结束后 LLM 自动压缩，生成结构化摘要（summary + keywords + time_period + atmosphere + valence + salience）。关键词从 keyword_pool 优先选择，确保长期收敛
- L2 事件提取层：未吸收 L1 ≥ 5 条或超 7 天触发，提取离散事件（含 8 个推断属性）。LLM 不可用时自动回退到规则式降级生成
- L3 人格画像层：surface/behavioral/core 三层分级，share 分级（private/trusted/public）控制 RAG 注入范围。Phase A 统计推断 + Phase B LLM 推断 + Phase C 增量漂移检测
- 冷启动流程：首次加载人格时自动注入知识背景
- 全量重建管线：支持切换 LLM 后端后从 L0 重新提取全部 L1/L2/L3

#### 三通道混合 RAG 检索

- BM25 全文检索：自研 Rust 实现，关键词精确匹配
- 向量检索：BruteForceIndex 暴力余弦 + 本地 ONNX 嵌入（bge-small-zh-v1.5）
- 知识图谱检索：BFS 遍历实体关系图，召回关联历史记忆
- RRF 倒数排名融合：三通道结果加权合并
- Ebbinghaus 遗忘曲线衰减：记忆检索权重随时间衰减，salience 调制衰减速度
- Persona-Aware 过滤：按人格画像 share 分级过滤可注入的记忆

#### 事件→性格推断管线

- Phase A 统计推断：高置信度事件特征均值计算
- Phase B LLM 推断：示例精选→聚类→推断→校准四步流水线
- Phase C 增量更新：计算 drift 漂移度，确认迁移路径
- 置信度追踪：每项特征关联 evidence，可溯源至原始事件

#### LLM Provider 层

- LM Studio 适配器：无 API Key，完全本地推理
- DeepSeek 适配器：支持 deepseek-v4
- OpenAI 适配器：兼容所有 OpenAI API 格式服务
- SSE 流式传输：futures channel + tokio spawn 异步架构
- OS 凭据管理器：Windows Credential Manager 安全存储 API Key
- 统一重试策略：指数退避，鉴权错误不重试

#### Tauri 2 桌面应用

- 原生 Windows 窗口：960×720 默认尺寸，最小 640×480
- 粉蓝双色设计系统：CSS Tokens 变量体系，暗/亮双主题
- 系统托盘：最小化到托盘，托盘菜单快捷操作
- 通知推送：新消息通知、后台处理完成通知
- 关闭确认弹窗：托盘最小化 / 完全退出二选一
- 配置向导：5 步引导（后端选择→API 配置→测试连接→人格选择→完成）
- 记忆查看器：L1/L2/L3 分页浏览，支持删除 + 二次确认
- UI 组件库：Toast / Modal / Skeleton / Markdown 渲染器
- Markdown 白名单 sanitizer + CSP + XSS 防护

#### CLI 工具

- 9 个子命令：`setup` / `ask` / `chat` / `memory` / `session` / `config` / `persona` / `index` / `export`
- 交互式 REPL：色彩输出，历史记录
- 流式输出：`--no-stream` 关闭流式，`--json` 输出原始 JSON
- 数据导出：支持 JSON / Markdown 格式
- 隐私确认：`--yes` 跳过确认

---

### 新增功能

#### 存储层

- 23 张表 SQLite schema，一次性 migration
- 19 个 Repository，手动行映射避免 sqlx derive 侵入 core
- WAL 模式，多连接读写并发
- 数据目录：默认 `%APPDATA%\Ramaria\`，环境变量 `RAMARIA_DATA_DIR` 覆盖

#### 配置管理

- 统一 RamariaConfig 配置结构
- SQLite settings 表持久化，支持 CLI 读取和修改
- 多后端配置（LM Studio / DeepSeek / OpenAI）
- 人格 TOML 文件（`config/personas/*.toml`）
- 隐私确认按 `provider + base_url` 粒度管理

#### 安全

- API Key 存储在 Windows Credential Manager
- 日志不记录完整对话内容，敏感字段截断或哈希
- CSP 内容安全策略，前端零 eval()
- Markdown 白名单标签 + 移除事件处理器 + 禁止危险协议
- CLI 路径穿越防护
- 本地模式完全离线，不发起外部网络请求

#### 错误处理

- 8 种错误变体（Config/Storage/Llm/Privacy/Index/Validation/Io/Unsupported）
- 错误到用户友好提示的映射（ErrorHint）
- CLI 错误上下文，带原文引用的错误信息

---

### 工程改善

#### 项目架构

- 7 个 crate 分层设计（core → storage/llm → memory → app → cli/desktop）
- Workspace resolver="3"，edition="2024"，MSRV 1.85
- 零 I/O 依赖的 core 层作为类型边界
- async-trait 抽象，支持 mock 测试

#### 测试

- 600+ 个测试函数，覆盖全部 7 个 crate
- 集成测试目录 `tests/`，含 fixture 数据和 mock backend
- CI：build + test + clippy(`-D warnings`) + fmt(`--check`)
- Smoke test 清单：11 类 83 项

#### 文档

- README：项目总览、架构图、模块职责表、分层记忆详解、核心创新设计
- 桌面使用指南：安装→配置→对话→记忆→设置→故障排除
- CLI 使用指南：9 个子命令完整参考
- 隐私说明：数据流向、安全措施、权限说明
- 发行说明模板：标准化 13 节结构
- 4 个 GitHub Issue 模板（Bug / Feature / Help / Config）

---

### 已知限制

- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件，正式图标待设计师提供
- 不支持 LLM 对话"重新生成"功能
- MCP Bridge / 导入器 / 自动更新 / 多角色 GUI 等功能已延后

---

## 版本历史概览

| 版本 | 日期 | 说明 |
|------|------|------|
| [v1.2.0](#120---2026-07-07) | 2026-07-07 | 深度打磨：Pipeline 架构重构 + L3 管线贯通 + 前端联动 + 后端修复 |
| [v1.1.0](#110---2026-06-16) | 2026-06-16 | 首个增量版本：记忆管线接通 + 嵌入模型 + QQ 导入器 |
| [v1.0.1](#101---2026-06-13) | 2026-06-13 | 紧急修复：全新安装无法启动 |
| [v1.0.0](#100---2026-06-12) | 2026-06-12 | Rust 重写完成，首个正式发布版本 |
| v0.7.0 | 2026-05-09 | Python 版最终功能版本（维护模式） |

> Python v0.3.x–v0.7.x 的完整变更记录见项目根目录 [`CHANGELOG.md`](../../CHANGELOG.md)。
