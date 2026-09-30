# ramaria-core

> 定位：核心类型边界（零 I/O）——配置、错误体系、抽象 trait、业务数据类型、锁与文本辅助。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§3.1 职责 / §4 数据与存储 / §18 参数基线）；代码级现状见 `../../../docs/architecture-ai-agent.md`。

## 职责

- **类型与 trait 的唯一声明地**：`StorageBackend`（= `StoreCrud` + `StoreInfrastructure`）、`LlmProvider`、`EmbeddingProvider`、`LlmResponseCache` 等。
- **统一错误体系**：`RamariaError` 分类（config / storage / llm / privacy / index / validation / io / unsupported）与 `RamariaResult<T>`，支持 `source` 错误链。
- **配置结构**：`RamariaConfig` 及各分组；数值默认值以 `config/default.toml` 为准（不在本层硬编码第二套口径）。
- **业务数据类型**：L0→L3 全链路数据（`Session` / `Message` / `MemoryL1` / `MemoryEvent` / `PersonalityTrait` / `PersonaFact` / `UttBlock` 等）。
- **并发与文本辅助**：锁恢复辅助（`lock`）、关键词 Newtype 与归一化（`keyword`）、文本工具（`text`）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `config` | `RamariaConfig` 及 `[session]` / `[retrieval]` / `[injection]` 等分组 |
| `error` | `RamariaError`、`RamariaResult`、分类构造器 |
| `traits` | `StoreCrud` / `StoreInfrastructure` / `StorageBackend` / `LlmProvider` / `EmbeddingProvider` / `ChatRequest` / `StreamDelta` |
| `types` | 全部业务数据类型与 `now_ms` / `new_id` 等基础函数 |
| `lock` | `lock_recover` / `read_recover` / `write_recover`（Poisoned 锁恢复） |
| `behavior` / `keyword` / `privacy` / `text` | 行为规则与反馈类型、关键词类型、隐私确认类型、文本辅助 |

## 相邻契约

- 依赖：**无**（仅标准库与 serde 等基础库）。**禁止**引入 sqlx / reqwest / tokio / 数据库 / 网络。
- 被依赖：全部 crate（storage / memory / llm / importer / service / mcp / cli / desktop）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增配置项 | `src/config.rs` | `config/default.toml` + 逐键比对测试 |
| 新增错误分类 | `src/error.rs` | 调用方映射（CLI 错误码 / 桌面提示） |
| 新增业务类型 | `src/types.rs` | `ramaria-storage/migrations/` 建表 + repo 行映射 |
| 修改/新增 trait 方法 | `src/traits.rs` | 全部实现方（`SqliteStorage`、各 provider、测试 mock） |
| 调整关键词类型 | `src/keyword.rs` | `ramaria-memory/src/keyword/`、倒排与 BM25 分词口径 |

## 验证

```bash
cargo test -j 2 -p ramaria-core
```
