# ramaria-service

> 定位：**唯一能力层**（与传输无关）——引擎装配 + 全部用例：召回 / 生成 / 写入 / 封存 / 空闲检查 / 人格读取。
> 上游 SSOT：`../../../docs/dev-2.2/v2.2-plan.md`（§4 总体设计 / §5 关键设计 + 附录 A 文件归属）、`../../../docs/dev/07-mcp/mcp-spec.md`（服务层边界）、`../../../docs/dev-2.2/test/contract-baseline.md`（对外契约）。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿；收敛后本 crate 吸收应用编排层能力）。

## 职责

- **装配层**：`Engine` 持有依赖（storage / LLM / 嵌入 / 配置 / 检索槽 / 策略 / 钩子）；`EngineOptions` 与 `from_parts` 支持生产装配与测试注入。
- **用例层**：`recall` / `chat` / `ingest` / `seal` / `idle` / `session` / `persona` 等；请求与响应为纯数据（`types`），不出现 stdio / Tauri / HTTP 概念。
- **并发与幂等**：封存抢占（`close_session_if_active` 原子条件更新，多进程同时封存只生成一份 L1）；空闲检查循环（`IdleLoop`）。
- **索引管理**：检索索引懒加载、代次刷新（跨进程写入可见）、L1 增量镜像、脏标记重建、刷新冷却窗口。
- **策略与钩子**：召回策略（`RecallPolicy`：人格白名单 / 原文开关）、封存钩子（`SealHooks`：行为 / 风格 / L2 触发）、封存许可（`set_seal_allowed`）。
- **降级纪律**：LLM / 嵌入不可用不阻塞装配与记忆读取；封存 L1 失败登记重试任务（`l1_summary_retry`）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `engine` | `Engine`（用例入口）/ `EngineOptions` / `from_parts` |
| `types` | `RecallRequest` / `ChatSendRequest` / `IngestRequest` / `SealOutcome` / `PersonaCardView` 等纯数据 |
| `recall` / `chat` / `ingest` / `seal` | 四条主用例实现（crate 内 `pub(crate) run`） |
| `idle` | `IdleLoop` / `IdleLoopOptions` / `tick` |
| `index` | 索引懒加载与增量镜像（`ensure_loaded` / `index_l1_into_mirrors`） |
| `hooks` | `default_seal_hooks`（轻量链，供 MCP 类宿主注册） |
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
| 召回策略 / 原文开关 | `src/recall.rs` 的 `RecallPolicy` | `[mcp].allow_raw_text` 等配置映射 |
| 封存链路 | `src/seal.rs` + `src/hooks.rs` | 钩子注册方（桌面完整链 / MCP 轻量链） |
| 索引行为 | `src/index.rs` | `[index].refresh_interval_seconds` + 跨进程可见性测试 |
| 空闲 / 调度 | `src/idle.rs`、`src/l2.rs`（收敛后 `lifecycle/`） | `[session].l1_idle_minutes`；MCP 不开 L2/L3 |
| 固定口径快照 | `tests/parity/` 四路径 + golden 更新（`PARITY_UPDATE_GOLDEN=1`） | 更新后需人工审阅 diff |

## 验证

```bash
cargo test -j 2 -p ramaria-service
cargo test -j 2 -p ramaria-service --test parity   # 平行对照基线
```
