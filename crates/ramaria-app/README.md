# ramaria-app（过渡层：结构收敛后移除）

> 定位：应用编排层——CLI 与桌面共用的用例编排（App、对话管线、会话生命周期、后台任务、状态机）。
> 上游 SSOT：`../../../docs/dev-2.2/v2.2-plan.md`（§2 背景与动机 / 附录 A 文件归属裁定表）、`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§3.1 / §3.2）。
> 状态：**待移除**（2.2 结构收敛把职责并入 `ramaria-service` 后删除本 crate，workspace 10 → 9）。

## 职责（收敛前的现状）

- `App`：依赖装配（storage / LLM / 嵌入 / keychain）与入口方法（send_message 流式、save_and_close_session、rebuild_retriever 等）。
- 对话管线：`app_chat.rs`（内联实现）与 `stages/`（含 5 个未接线 stage）；`pipeline.rs`（trait 化抽象）。
- 会话生命周期：`session_lifecycle/`（活跃指针、空闲检测线程、L2/L3 定时调度、shutdown）。
- 检索器维护：`app_retriever.rs`（视图收集、临时实例替换、BM25 迁移、重建失败告警位）。
- 配置双写：`config_sync.rs`（config.toml ↔ settings / backend_config）。
- 其余：`app_state` / `app_setup` / `app_style` / `app_knowledge` / `app_fact_extract` / `app_privacy` / `diagnostics` / `model_manager` / `update` / `feedback` / `eta` / `error_hint` / `stream_event` / `bridge`。

## 收敛去向（速查）

| 源 | 去向 |
|----|------|
| `app.rs`（App 本体） | `ramaria-service/src/engine.rs`（能力切 Lifecycle / Indexer / ConfigWriter） |
| `app_chat.rs` + `pipeline.rs` + `stages/` | `ramaria-service/src/chat.rs`（归一为 `step_*`；pipeline 与 stage 删除） |
| `app_retriever.rs` | `ramaria-service/src/index.rs`（合并为 Indexer） |
| `session_lifecycle/` | `ramaria-service/src/lifecycle/` |
| `config_sync.rs` | `ramaria-service/src/config.rs`（ConfigWriter） |
| `stream_event.rs` | `ramaria-service/src/stream_event.rs`（CLI / 桌面共用契约） |
| `persona_prompt.rs` | 删除（调用方直连 `ramaria_memory::chat`） |
| 宿主专属（Tauri 壳 / 托盘 / 通知 / 路径脱敏 / 输出格式化 / 探针） | 留在 `ramaria-desktop` / `ramaria-cli` |

逐文件裁定见 `../../../docs/dev-2.2/v2.2-plan.md` 附录 A。

## 相邻契约

- 依赖：`ramaria-core` / `ramaria-storage` / `ramaria-memory` / `ramaria-llm`（+ `ramaria-importer`，feature gate）。
- 被依赖：`ramaria-cli` / `ramaria-desktop`（收敛后改为直连 `ramaria-service`）。
- 与 `ramaria-service` **互不依赖**（收敛期两侧并存，M4 后仅保留 service）。

## 常见改动落点（收敛期纪律）

| 场景 | 要求 |
|------|------|
| 本版新增能力 | **一律落在 `ramaria-service`**，不要在本 crate 添加新实现（避免双轨扩大） |
| 修复线上缺陷 | 最小修复；若同时需改 service，两侧口径必须一致（对照测试锁定） |
| 契约面 | 不得改动（见 `../../../docs/dev-2.2/test/contract-baseline.md`） |

## 验证

```bash
cargo test -j 2 -p ramaria-app
```
