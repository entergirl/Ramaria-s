# ramaria-mcp

> 定位：MCP 协议壳（stdio）——工具 schema、参数校验、结果包装与错误映射；不含业务逻辑。
> 上游 SSOT：`../../../docs/dev/07-mcp/mcp-spec.md`（工具契约 / 门禁 / 并发约束）、`../../../docs/dev-2.3/test/contract-baseline-2.3.md`（§5 工具基线）。

## 职责

- **协议实现**：基于 rmcp（锁定版本）的 stdio 服务；6 个工具：`memory_recall` / `chat_send` / `chat_ingest` / `persona_list` / `persona_get` / `chat_history`。
- **参数层**：`params.rs` 的 JsonSchema 入参类型与枚举（`LayerParam` / `SectionParam` / `MessageParam`），只做形状校验与默认值归一。
- **结果层**：成功走 `structured`（content 文本 JSON + structuredContent 同值）；失败走 `structured_error`（`isError=true`，结果内错误，不用协议级错误）。
- **门禁**：`[mcp].enabled` 总开关、`[mcp].allow_ingest` 写开关、人格白名单前置校验。
- **stdout 纪律**：stdout 仅协议消息；日志一律走 stderr（`init_stderr_logging`）。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/lib.rs` | crate 根：模块声明与 re-export | — |
| `src/host.rs` | `serve_stdio` / `McpHostOptions` / `init_stderr_logging`（宿主启动面：装配 → 注入门禁与钩子 → 拉起空闲循环 → stdio → 优雅关停） | 集成 `tests/protocol_e2e.rs` |
| `src/server.rs` | `RamariaMcpServer`（`tool_router()` + `#[rmcp::tool_handler]`、门禁与白名单） | 集成 `tests/protocol_e2e.rs` |
| `src/params.rs` | 入参 schema（JsonSchema 派生）与 wire 参数类型 | 内联 |
| `src/result.rs` | 结果包装与错误映射（`structured` / `structured_error`） | 内联 |
| `src/tools/` | 六个工具实现（`recall` / `chat` / `ingest` / `persona` / `history`）：参数转换 + 调服务层用例 + 结果包装 | 集成 `tests/protocol_e2e.rs` |
| `tests/protocol_e2e.rs` | 内存 stdio 协议端到端（握手 → `tools/list` → 工具调用 → 结果包装 → 开关门禁 / 双服务共库） | `tests/protocol_e2e.rs` |

## 相邻契约

- 依赖：`ramaria-service`（用例）+ `ramaria-core`（类型 / 错误）。
- **禁止**：业务逻辑（检索 / 封存 / 装配一律经 `ramaria-service`）、`tauri`。
- 宿主差异：MCP 注册轻量封存钩子链（`default_seal_hooks`）、维持空闲检查（`IdleLoop`），不开启 L2/L3 常驻调度（决策基线 §3.2）。
- **门禁注入顺序**：宿主必须在构造 server 前注入召回策略 / 封存许可，否则门禁默认放行（见 `tests/protocol_e2e.rs` 的 `start_server` 说明）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 / 修改工具 | `src/tools/<name>.rs` + `src/server.rs` 的 `tool_router()` | `../../../docs/dev/07-mcp/mcp-spec.md` + 契约基线 §5 + 协议测试断言 |
| 入参字段 / 默认值 | `src/params.rs` | 与服务层 `types/` 口径对齐（避免双处定义） |
| 错误映射 | `src/result.rs` | 工具错误一律结果内返回 |
| 门禁策略 | `src/server.rs` | `[mcp]` 配置（`config/default.toml`） |

## 验证

```bash
cargo test -j 2 -p ramaria-mcp
```
