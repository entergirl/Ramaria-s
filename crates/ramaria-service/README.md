# ramaria-service

> 定位：**唯一能力层**（与传输无关）——引擎装配 + 生命周期 / 索引 / 配置 + 全部用例：召回 / 生成 / 写入 / 封存 / 浏览 / 人格。
> 上游 SSOT：`../../../docs/dev-2.2/v2.2-plan.md`（§4 总体设计 / §5 关键设计 + 附录 A 文件归属）、`../../../docs/dev/07-mcp/mcp-spec.md`（服务层边界）、`../../../docs/dev-2.2/test/contract-baseline.md`（对外契约）。

## 职责

- **装配层**：`Engine` 持有依赖（storage / LLM / 嵌入 / 配置快照 / 检索槽 / 策略 / 钩子）；`EngineOptions` 与 `from_parts` 支持生产装配与测试注入；配置以 `RwLock` 快照持有，装配路径只读。
- **能力层**：`Lifecycle`（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）；索引构建（`index.rs`：懒加载 / 代次刷新 / 增量镜像 / 重建）；`ConfigWriter`（配置双写与一致性校验）；状态机（NeedsSetup → Indexing → Ready）。
- **用例层**：`recall` / `chat`（含流式）/ `ingest` / `seal` / `session` / `settings` / `browse` / `persona` / `behavior` / `style` / `keyword` / `export` / `utt` / `model` / `setup` / `diagnostics` / `update` / `import`（`importer` feature）等；请求与响应为纯数据（`types`），不出现 stdio / Tauri / HTTP 概念。
- **并发与降级**：封存抢占（原子条件更新，多进程同时封存只生成一份 L1）；封存钩子轻量 / 完整两套默认装配（宿主注册制）；LLM / 嵌入不可用不阻塞装配与记忆读取；流式事件（`stream_event`）为 CLI 与桌面共用契约。

## 公共入口

| 模块 | 内容 |
|------|------|
| `engine` | `Engine`（用例入口）/ `EngineOptions` / `from_parts` / 配置用例（`load_full_config` / `reload_config` / `save_config` / `sync_backend_config`） |
| `config` | `ConfigWriter`（双写 / 一致性校验 / 模板生成 / 原子写入）/ `SyncOutcome` / `MismatchEntry` |
| `types` | `RecallRequest` / `ChatSendRequest` / `IngestRequest` / `SealOutcome` / `PersonaCardView` 等纯数据 |
| `recall` / `chat` / `ingest` / `seal` | 四条主用例；`chat` 流式入口经 `Engine::chat_stream` + `stream_event` 暴露 |
| `lifecycle` / `idle` | `Lifecycle` / `LifecycleOptions`（`l1` 重生成 / `l2_l3` 调度 / `idle` 空闲检查）；`IdleLoop`（MCP 轻量循环） |
| `index` | 索引懒加载 / 显式重建 / L1 增量镜像 / 重建失败告警位 |
| `hooks` | `default_seal_hooks`（轻量链）/ `full_seal_hooks`（完整链：L2 提取 + 知识抽取 + L3 级联） |
| `session` / `settings` / `browse` / `persona` | 会话增删与历史 / 设置与 schema 版本 / 记忆与会话浏览 / 人格 |
| `behavior` / `style` / `keyword` / `export` / `utt` | 行为规则 / 风格统计 / 关键词词典 / 导出数据装配 / 话语块重建 |
| `model` / `setup` / `diagnostics` / `update` | 模型管理 / 首次配置 / 诊断导出 / 版本检查编排 |
| `privacy` / `bridge` / `feedback` / `l2` / `fact_extract` / `eta` / `error_hint` | 生成伴随能力与辅助用例（隐私确认 / 桥接 / 弱反馈 / L2 触发 / 事实抽取 / 进度预估 / 提示映射） |
| `import` | QQ 导入用例（`importer` feature）；解析实现见 `ramaria-importer` |

## 相邻契约

- 依赖：`ramaria-core` / `ramaria-storage` / `ramaria-memory` / `ramaria-llm`（`embedding-native`）；`ramaria-importer`（optional，`importer` feature）。
- **禁止依赖任何入口层**：`ramaria-cli` / `ramaria-desktop` / `tauri`（编译期约束）。
- 被依赖：`ramaria-mcp`（协议壳）、`ramaria-cli` / `ramaria-desktop`（业务命令全部经本层用例）。
- 宿主差异一律由装配选项 / 策略 / 钩子表达（D-V22-011），不写第二份代码。

## 测试基建

- `src/test_support.rs` 单元脚手架（真实 SQLite + mock LLM + 造数）；`tests/suites/` 集成套件（装配 / 封存与会话生命周期 / 提示词 / 知识 / 行为 / 推断等）；`tests/parity/` golden 基线（封存 / 召回 / chat / 索引四路径，`PARITY_UPDATE_GOLDEN=1` 更新）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增用例 | `src/<usecase>.rs` + `engine.rs` 暴露方法 + `types.rs` 请求/响应 | MCP 工具或入口命令（如需对外） |
| 配置双写 / 模板 | `src/config.rs` + `config/default.toml` | 新增配置组必须同步模板（逐键比对测试） |
| 召回策略 / 原文开关 | `src/recall.rs` 的 `RecallPolicy` | `[mcp].allow_raw_text` 等配置映射 |
| 封存链路 / 钩子 | `src/seal.rs` + `src/hooks.rs` | 钩子注册方按宿主选链（MCP 轻量 / 桌面完整） |
| 索引 / 空闲调度 | `src/index.rs`；`src/lifecycle/{idle,l2_l3}.rs`（长驻）/ `src/idle.rs`（轻量） | `[index]` / `[session]` 配置；MCP 不开 L2/L3 |
| 会话生命周期 | `src/lifecycle/mod.rs`（活跃指针 / 关停）+ `src/session.rs`（增删 / 历史） | 入口装配 `Engine::start_lifecycle(LifecycleOptions)`；封存抢占语义 |
| 固定口径快照 | `tests/parity/`（golden 更新需人工审阅 diff） | `--test parity` |

## 验证

```bash
cargo test -j 2 -p ramaria-service                      # 单元；集成套件加 --test suites
cargo test -j 2 -p ramaria-service --test parity        # golden 基线
cargo test -j 2 -p ramaria-service --features importer  # 导入用例
```
