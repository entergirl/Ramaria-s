# ramaria-importer

> 定位：聊天记录导入（当前仅 QQ Chat Exporter v6.x JSON）——解析、切分、画像归属、写库。
> 上游 SSOT：`../../../docs/dev/05-import/import-spec.md`（导入规格与模式）、`../../../docs/dev/05-import/import-qq-schema.md`（JSON 结构参考）。

## 职责

- **格式解析**：QQ Chat Exporter v6.x JSON 识别与容错解析（`qq/parser/`）。
- **会话切分**：按时间间隔（`gap`）切分为会话；快速 / 深度双模式口径。
- **画像归属**：我方 / 对方（self / other）双画像映射与 `persona_uid` 绑定。
- **幂等写入**：按消息指纹去重写入（`writer`）；导入统计与报告所需计数。
- **调用入口**：经 `feature = "importer"` 由服务层用例启用；入口（CLI / 桌面）以该 feature 装配服务层。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/lib.rs` | crate 根：模块声明与 re-export | — |
| `src/traits.rs` | 导入器抽象（`pub use traits::{...}`） | 内联 |
| `src/error.rs` | 导入错误类型 | 内联 |
| `src/writer.rs` | 落库写入器（含指纹去重、批量写入与统计） | `src/writer/tests.rs` |
| `src/qq/mod.rs` | QQ 导入门面与中间结构（`QqChatExporter` v6.x） | 集成 `tests/qq_parser_tests.rs` |
| `src/qq/parser/` | 解析实现：`detect`（格式识别）/ `time`（时间解析）/ `elements`（元素与消息类型）/ `message`（单消息转换）/ `sessions`（会话切分）/ `stream`（流式大文件解析） | `src/qq/parser/tests.rs` |
| `tests/qq_parser_tests.rs` | 解析器端到端（真实导出样本形态 → 会话与消息） | `tests/qq_parser_tests.rs` |

## 相邻契约

- 依赖：`ramaria-core`（类型 / 错误）与存储（经 trait）。
- 被依赖：`ramaria-service`（导入用例，`importer` feature）；`ramaria-cli` / `ramaria-desktop`（导入侧解析与过滤枚举）。
- **禁止**：UI 依赖。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 解析新格式版本 | `src/qq/parser/` | `../../../docs/dev/05-import/import-spec.md` + 解析测试夹具 |
| 切分策略 | `src/qq/parser/sessions.rs`（`split_into_sessions`，`gap` 口径） | CLI `import qq --gap` 默认值口径 |
| 去重 / 指纹 | `src/writer.rs` | `messages.import_fingerprint`（SHA-256 前 16 位） |
| 画像映射 | `src/qq/parser/`（我方 / 对方归属逻辑） | 桌面导入命令参数（`self_*` / `other_*`） |

## 验证

```bash
cargo test -j 2 -p ramaria-importer
```
