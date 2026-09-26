# ramaria-service

> 定位：**唯一能力层**（与传输无关）——引擎装配 + 全部用例：召回 / 生成 / 写入 / 封存 / 空闲检查 / 人格读取。
> 上游 SSOT：`../../../docs/dev-2.2/v2.2-plan.md`（§4 总体设计 / §5 关键设计 + 附录 A 文件归属）、`../../../docs/dev/07-mcp/mcp-spec.md`（服务层边界）、`../../../docs/dev-2.2/test/contract-baseline.md`（对外契约）。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿；收敛后本 crate 吸收应用编排层能力）。

## 职责

- **装配层**：`Engine` 持有依赖（storage / LLM / 嵌入 / 配置快照 / 检索槽 / 策略 / 钩子）；`EngineOptions` 与 `from_parts` 支持生产装配与测试注入；配置以 `RwLock` 快照持有，装配路径只读。
- **用例层**：`recall` / `chat` / `ingest` / `seal` / `idle` / `session` / `persona` 等；请求与响应为纯数据（`types`），不出现 stdio / Tauri / HTTP 概念。
- **配置双写**：`ConfigWriter` 统一管理 config.toml ↔ settings / backend_config 表（一致性校验以文件为准、模板生成、原子写入）；引擎配置用例（`load_full_config` / `reload_config` / `save_config` / `sync_backend_config`）落盘成功后热重载配置快照。
- **并发与幂等**：封存抢占（`close_session_if_active` 原子条件更新，多进程同时封存只生成一份 L1）；空闲检查循环（轻量 `IdleLoop`）；会话生命周期容器（`Lifecycle`：活跃指针 / 空闲检查 / L2-L3 调度 / 优雅关停）。
- **索引管理**：检索索引懒加载、代次刷新（跨进程写入可见）、L1 增量镜像、脏标记重建、刷新冷却窗口、显式全量重建（`rebuild_index`）与重建失败告警位（`is_index_rebuild_failed`）。
- **策略与钩子**：召回策略（`RecallPolicy`：人格白名单 / 原文开关）、封存钩子（`SealHooks`：行为 / 风格 / L2 触发；提供轻量与完整两套默认装配，钩子不持有引擎）、封存许可（`set_seal_allowed`）。
- **降级纪律**：LLM / 嵌入不可用不阻塞装配与记忆读取；封存 L1 失败登记重试任务（`l1_summary_retry`）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `engine` | `Engine`（用例入口）/ `EngineOptions` / `from_parts` / 配置用例（`load_full_config` / `reload_config` / `save_config` / `sync_backend_config`） |
| `config` | `ConfigWriter`（双写 / 一致性校验 / 模板生成 / 原子写入）/ `SyncOutcome` / `SyncWriteResult` / `MismatchEntry` |
| `types` | `RecallRequest` / `ChatSendRequest` / `IngestRequest` / `SealOutcome` / `PersonaCardView` 等纯数据 |
| `recall` / `chat` / `ingest` / `seal` | 四条主用例实现（crate 内 `pub(crate) run`）；`chat` 同时提供流式入口（`stream`，经 `Engine::chat_stream` 暴露） |
| `stream_event` | 流式事件领域模型（`StreamEvent`：delta / done / error）与事件流句柄（`ChatStreamHandle` / `ChatEventStream`） |
| `privacy` / `bridge` / `feedback` | 生成编排的伴随能力：隐私确认 / 新会话桥接 / 弱反馈检测 |
| `idle` | `IdleLoop` / `IdleLoopOptions` / `tick`（无生命周期宿主的轻量循环） |
| `lifecycle` | `Lifecycle` / `LifecycleOptions`（活跃指针 / 手动关闭 / 空闲阈值热更新 / 循环句柄 / 关停）；`lifecycle::l1`（`regenerate_l1*` 与补扫）/ `lifecycle::l2_l3`（L2 触发与 L3 推断调度） |
| `index` | 索引懒加载 / 显式重建 / 增量镜像（`ensure_loaded` / `rebuild` / `index_l1_into_mirrors`） |
| `hooks` | `default_seal_hooks`（轻量链：单 persona 最小 L2 触发，供 MCP 类宿主注册）/ `full_seal_hooks`（完整链：全 persona L2 提取 + 知识抽取 + L3 级联，供桌面类长驻宿主注册） |
| `fact_extract` | 知识事实自动抽取（`auto_fact_detect` 增强层，L2 提取成功后调用） |
| `persona` / `l2` / `session` | 人格读取、L2 触发、会话历史 |

## 相邻契约

- 依赖：`ramaria-core` / `ramaria-storage` / `ramaria-memory` / `ramaria-llm`（embedding-native）。
- **禁止依赖任何入口层**：`ramaria-app` / `ramaria-cli` / `ramaria-desktop` / `tauri`（编译期约束）。
- 被依赖：`ramaria-mcp`（协议壳）、`ramaria-cli` / `ramaria-desktop`（收敛后直连）。
- 宿主差异一律由装配选项 / 策略 / 钩子表达（不写第二份代码）。

## 测试基建

| 位置 | 内容 |
|------|------|
| `src/test_support.rs` | 单元测试脚手架（crate 内 `#[cfg(test)]`）：真实 SQLite + 最小 LLM mock + 造数 |
| `tests/parity/` | 平行对照基线：封存 / 召回 / chat / 索引四条路径的规范化快照与 golden（`tests/parity/golden/`） |

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增用例 | `src/<usecase>.rs` + `engine.rs` 暴露方法 + `types.rs` 请求/响应 | MCP 工具或入口命令（如需对外） |
| 配置双写 / 模板 | `src/config.rs` + `config/default.toml` | 新增配置组必须同步模板（有逐键比对测试）与双写覆盖 |
| 召回策略 / 原文开关 | `src/recall.rs` 的 `RecallPolicy` | `[mcp].allow_raw_text` 等配置映射 |
| 封存链路 | `src/seal.rs` + `src/hooks.rs` | 钩子注册方按响应语义选装配（MCP 用轻量链 / 桌面等长驻宿主用完整链） |
| 索引行为 | `src/index.rs` | `[index].refresh_interval_seconds` + 跨进程可见性测试 |
| 空闲 / 调度 | `src/lifecycle/{idle,l2_l3,mod}.rs`（长驻宿主）；`src/idle.rs`（轻量循环） | `[session].l1_idle_minutes` / `[session].l2_check_interval_seconds`；MCP 不开 L2/L3 |
| 会话生命周期 | `src/lifecycle/mod.rs`（活跃指针 / 手动关闭 / 关停） | 入口装配用 `Engine::start_lifecycle(LifecycleOptions)`；`seal` 抢占语义 |
| 知识事实抽取 | `src/fact_extract.rs` | `[knowledge].auto_fact_detect` 开关；L2 提取成功后触发 |
| 固定口径快照 | `tests/parity/` 四路径 + golden 更新（`PARITY_UPDATE_GOLDEN=1`） | 更新后需人工审阅 diff |

## 验证

```bash
cargo test -j 2 -p ramaria-service
cargo test -j 2 -p ramaria-service --test parity   # 平行对照基线
```
