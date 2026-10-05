# ramaria-service

> 定位：**唯一能力层**（与传输无关）——引擎装配 + 生命周期 / 索引 / 配置 + 全部用例：召回 / 生成（含主动对话）/ 写入 / 封存 / 浏览 / 人格。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§3.2 边界 / §9 记忆系统）、`../../../docs/dev/07-mcp/mcp-spec.md`（服务层边界）、`../../../docs/dev-2.3/test/contract-baseline-2.3.md`（对外契约）。

## 职责

- **装配层**：`Engine` 持有依赖（storage / LLM / 嵌入 / 配置快照 / 检索槽 / 策略 / 钩子）；`EngineOptions` 与 `from_parts` 支持生产装配与测试注入；配置以 `RwLock` 快照持有，装配路径只读。
- **能力层**：`Lifecycle`（活跃指针 / 空闲检查 / L2-L3 调度 / 主动对话循环 / 关停）；索引构建（`index`：懒加载 / 代次刷新 / 增量镜像 / 重建）；`ConfigWriter`（配置双写与一致性校验）；状态机（NeedsSetup → Indexing → Ready / Degraded / FatalError）。
- **用例层**：`recall` / `chat`（含流式）/ `proactive`（主动对话调度与投放）/ `ingest` / `seal` / `session` / `settings` / `browse` / `persona` / `behavior` / `style` / `keyword` / `export` / `utt` / `model` / `setup` / `diagnostics` / `update` / `import`（`importer` feature）等；请求与响应为纯数据（`types`），不出现 stdio / Tauri / HTTP 概念。
- **并发与降级**：封存抢占（原子条件更新，多进程同时封存只生成一份 L1）；封存钩子轻量 / 完整两套默认装配（宿主注册制）；LLM / 嵌入不可用不阻塞装配与记忆读取；流式事件（`stream_event`）为 CLI 与桌面共用契约。

## 文件地图（目录 → 职责）

> 测试约定：生产文件旁的同名 `xxx/tests.rs` 为该模块单测；`tests/` 为集成目标（`suites` / `parity` / `entrypoints`）。

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/lib.rs` | crate 根：模块声明与 re-export；全部用例的挂载点入口 | — |
| `src/engine/` | `Engine` 门面与装配：`assemble`（`open_with` 装配顺序）/ `index_state`（状态机）/ `usecases_{memory,browse,persona,proactive,ops}`（按域拆的用例块） | `src/engine/tests.rs` |
| `src/types/` | 用例层纯数据：`chat` / `recall` / `ingest` / `persona` / `setup` / `memory_browse` / `facts` / `session` / `keyword` / `defaults` | `src/types/tests.rs` |
| `src/config/` | `ConfigWriter` 配置双写：`file_io`（原子写）/ `db_io` / `merge` / `flatten` / `backend_map` / `results` | `src/config/tests.rs` |
| `src/chat/` | 对话用例：`steps`（`prepare_request` 前置编排）/ `context`（历史窗口与上下文）/ `generate`（LLM 调用与流式）/ `proactive`（主动生成，assistant-only 非流式） | `src/chat/tests.rs` |
| `src/proactive/` | 主动对话域：`schedule`（触发链 + `quiet` 免打扰 + `gates` 资格闸门）/ `switch`（人格开关三态）/ `roster`（名单读写用例）/ `picker`（`sources` 四源选题与打分）/ `judge`（AI 判据）/ `activity`（活跃时段统计）/ `state`（状态键）/ `sink`（投放注册制）；指令与结果形态 `topic` | `src/proactive/tests.rs`、`schedule/tests.rs`、`switch/tests.rs`、`roster/tests.rs`、`picker/tests.rs`、`judge/tests.rs`、`state/tests.rs`、`sink/tests.rs`、`activity/tests.rs` |
| `src/recall/` | 分层召回装配：`policy`（闸门）/ `layers` / `search` / `overview` / `entry` | `src/recall/tests.rs` |
| `src/browse/` | 记忆与会话浏览：`l1` / `l2_l3` / `profile` / `evidence` / `facts` / `session` / `channel` / `view` | `src/browse/tests.rs` |
| `src/persona/` | 人格用例：`load` / `regenerate` / `update` / `view` | `src/persona/tests.rs` |
| `src/diagnostics/` | 诊断导出：`collect` / `redact`（二次脱敏）/ `render` / `export` | `src/diagnostics/tests.rs` |
| `src/import/` | QQ 导入用例（`importer` feature）：`detect` / `analyze` / `l0` / `l1` / `deep` | `src/import/tests.rs` |
| `src/lifecycle/` + `src/lifecycle/l2_l3/` | 生命周期容器：`container` / `options` / `idle`（空闲检查）/ `l1`（摘要重生成与补扫）；`l2_l3/`：`l2` / `l3` / `schedule` / `unbound` | `src/lifecycle/tests.rs`、`l2_l3/tests.rs` |
| `src/test_support/` | `#[cfg(test)]` 单测脚手架：`llm` / `embedding` / `storage`（真实 SQLite + migration）/ `engine` / `seed` | 自身即测试基建 |
| `src/seal.rs` | 封存链路：抢占 → L1 → 索引镜像 → utt → examples → 宿主钩子 | 内联 |
| `src/hooks.rs` | 封存钩子装配：`default_seal_hooks`（轻量）/ `full_seal_hooks`（完整链） | 内联 |
| `src/index.rs` → `src/index/tests.rs` | 索引懒加载 / 显式重建 / 代次刷新 / 重建失败告警位（重建含 BM25 / 向量 / 图谱通道同批构建） | `src/index/tests.rs` |
| `src/ingest.rs` → `src/ingest/tests.rs` | 外部对话回流写入（去重 / 会话桥接） | `src/ingest/tests.rs` |
| `src/idle.rs` → `src/idle/tests.rs` | `tick` + `IdleLoop`（MCP 轻量宿主循环） | `src/idle/tests.rs` |
| `src/session.rs` / `src/settings.rs` | 会话增删与历史 / 设置与 schema 版本 | 内联 |
| `src/behavior.rs` / `src/style.rs` / `src/keyword.rs` | 行为规则裁决 / 风格统计用例 / 关键词词典与别名裁决 | `behavior/tests.rs`、`keyword/tests.rs`；`style.rs` 内联 |
| `src/export.rs` / `src/utt.rs` / `src/model.rs` | 导出数据装配（`EXPORT_FORMAT_VERSION`）/ 话语块重建 / 模型管理编排 | `export/tests.rs`、`model/tests.rs`；`utt.rs` 内联 |
| `src/setup.rs` / `src/update.rs` | 首次配置（含 `SetupStatus`）/ 版本检查 | `src/setup/tests.rs`；`update.rs` 内联 |
| `src/feedback.rs` / `src/privacy.rs` / `src/bridge.rs` | 弱反馈 S2/S3 / 隐私确认 / 会话桥接 | `feedback/tests.rs`；`privacy.rs`、`bridge.rs` 内联 |
| `src/fact_extract.rs` / `src/l2.rs` / `src/eta.rs` | 事实抽取触发 / L2 触发 / 导入进度预估 | 内联 |
| `src/error_hint.rs` / `src/stream_event.rs` | 入口错误映射唯一出处 / 流式事件契约 | 内联 |
| `tests/suites/` | 端到端集成套件（装配 / 封存与会话生命周期 / 提示词 / 知识 / 行为 / 推断等）+ 冒烟自检；`support/mock_backend/` 提供 `MockStorage` / `MockLlm` | `tests/suites/` |
| `tests/parity/` | 四条关键路径（封存 / 召回 / chat / 索引）golden 基线对照；`support/` 提供环境与快照 | `tests/parity/`（`PARITY_UPDATE_GOLDEN=1` 更新需人工审阅） |
| `tests/entrypoints/` | 三入口装配形态（桌面完整链 / CLI 默认 / MCP 轻量）在同库组合下的行为：并发封存抢占、跨引擎回流可见、缺索引自愈、门禁 | `tests/entrypoints/` |

## 相邻契约

- 依赖：`ramaria-core` / `ramaria-storage` / `ramaria-memory` / `ramaria-llm`（`embedding-native`）；`ramaria-importer`（optional，`importer` feature）。
- **禁止依赖任何入口层**：`ramaria-cli` / `ramaria-desktop` / `tauri`（编译期约束）。
- 被依赖：`ramaria-mcp`（协议壳）、`ramaria-cli` / `ramaria-desktop`（业务命令全部经本层用例）。
- 宿主差异一律由装配选项 / 策略 / 钩子表达（决策基线 §1.5），不写第二份代码。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增用例 | `src/<usecase>.rs|/` + `engine/usecases_*.rs` 暴露方法 + `types/` 请求/响应 | MCP 工具或入口命令（如需对外） |
| 配置双写 / 模板 | `src/config/` + `config/default.toml` | 新增配置组必须同步模板（逐键比对测试） |
| 召回策略 / 原文开关 | `src/recall/` 的 `RecallPolicy` | `[mcp].allow_raw_text` 等配置映射 |
| 封存链路 / 钩子 | `src/seal.rs` + `src/hooks.rs` | 钩子注册方按宿主选链（MCP 轻量 / 桌面完整） |
| 主动对话（调度 / 选题 / 判据 / 投放） | `src/proactive/` + `src/chat/proactive.rs` | `[proactive]` 配置；提示词 `ramaria-memory/src/prompt/builder/proactive.rs`；桌面 sink 与事件桥（宿主注册） |
| 索引 / 空闲调度 | `src/index.rs`；`src/lifecycle/`（长驻）/ `src/idle.rs`（轻量） | `[index]` / `[session]` 配置；MCP 不开 L2/L3 |
| 会话生命周期 | `src/lifecycle/`（活跃指针 / 关停）+ `src/session.rs`（增删 / 历史） | 入口装配 `Engine::start_lifecycle(LifecycleOptions)`；封存抢占语义 |
| 固定口径快照 | `tests/parity/`（golden 更新需人工审阅 diff） | `--test parity` |

## 验证

```bash
cargo test -j 2 -p ramaria-service                      # 单元；集成套件加 --test suites
cargo test -j 2 -p ramaria-service --test parity        # golden 基线
cargo test -j 2 -p ramaria-service --features importer  # 导入用例
```
