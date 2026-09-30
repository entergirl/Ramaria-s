# ramaria-importer

> 定位：聊天记录导入（当前仅 QQ Chat Exporter v6.x JSON）——解析、切分、画像归属、写库。
> 上游 SSOT：`../../../docs/dev/05-import/import-spec.md`（导入规格与模式）、`../../../docs/dev/05-import/import-qq-schema.md`（JSON 结构参考）。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿）。

## 职责

- **格式解析**：QQ Chat Exporter v6.x JSON 识别与容错解析（`qq/`）。
- **会话切分**：按时间间隔（`gap`）切分为会话；快速 / 深度双模式口径。
- **画像归属**：我方 / 对方（self / other）双画像映射与 `persona_uid` 绑定。
- **幂等写入**：按消息指纹去重写入（`writer`）；导入统计与报告所需计数。
- **调用入口**：经 `feature = "importer"` 由服务层 / 入口层按需启用（CI 默认不打包）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `qq` | 解析器与中间结构（`QqChatExporter` v6.x） |
| `writer` | 落库写入器（含指纹去重） |
| `traits` | 导入器抽象（`pub use traits::{...}`） |
| `error` | 导入错误类型 |

## 相邻契约

- 依赖：`ramaria-core`（类型 / 错误）与存储（经 trait）。
- 被依赖：`ramaria-service`（导入用例），以 `importer` feature gate 启用。
- **禁止**：UI 依赖。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 解析新格式版本 | `src/qq/` | `import-spec.md` + 解析测试夹具 |
| 切分策略 | 会话切分实现（`gap` 口径） | CLI `import qq --gap` 默认值口径 |
| 去重 / 指纹 | `src/writer.rs` | `messages.import_fingerprint`（SHA-256 前 16 位） |
| 画像映射 | 我方 / 对方归属逻辑 | 桌面导入命令参数（`self_*` / `other_*`） |

## 验证

```bash
cargo test -j 2 -p ramaria-importer
```
