# ramaria-storage

> 定位：SQLite 存储层——schema（27 张表基线 + 增量迁移）、Repository 模式 CRUD、索引存取、重试。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§4 数据与存储）；表结构与接口现状见 `../../../docs/architecture-ai-agent.md`。

## 职责

- **migration 管理**：`database::init_pool` 建池并执行 `migrations/` 全量迁移；WAL、外键、busy_timeout（多进程共库等待写锁而非失败）。
- **Repository 实现**：每个实体一个 `repo/*.rs`，负责 SQL 与行映射；`SqliteStorage` 聚合实现 `StoreCrud` + `StoreInfrastructure`。
- **事务与一致性**：事件批量写入（`save_event_batch`）、会话级联删除（`delete_session_cascade`）等单事务语义。
- **索引与缓存存取**：BM25 索引、关键词池/引用、LLM 响应缓存、后台任务（`background_jobs`）。
- **写锁重试**：`retry` 模块提供多进程/多线程共库下的写冲突重试辅助。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/lib.rs` | crate 根：声明与 re-export（委托层已外移，本文件保持薄） | — |
| `src/database.rs` | `init_pool` / `init_pool_with` / `PoolTuning`（migration 入口） | 内联 |
| `src/retry.rs` | 写冲突重试辅助（`SQLITE_BUSY` / `SQLITE_LOCKED` 有限重试） | 内联 |
| `src/backend/` | `SqliteStorage` 的 trait 实现：`crud`（实体读写）/ `infrastructure`（索引 / 任务 / 设置）/ `llm_cache` | 内联 + `src/tests/` |
| `src/repo/` | 每实体一文件：`sessions` / `messages` / `memory_l1` / `events` / `facts` / `traits` / `keyword` / `cluster` / `behavior_rules` / `examples` / `style_stats` / `personas` / `settings` / `schema_meta` / `background_jobs` / `feedback_log` / `llm_response_cache` / `l2_fingerprint` / `backend_config` / `privacy_consent` / `utt_blocks` / `traits.rs`（repo 共用辅助） | `repo/<entity>/tests.rs`（keyword / messages / sessions）或内联 |
| `src/tests/` | 单元测试按域拆分：`mod.rs`（共享夹具）+ schema / session / message / memory_l1 / event / persona / personality_trait / fact / keyword / index / utt_block / example / llm_cache / background_job | `src/tests/` |
| `migrations/` | `20260905_v2.0_schema.sql`（基线 27 表）+ `20260918_v2.1_channels.sql` + `20261001_v2.3_index_version.sql` + `20261002_v2.4_proactive.sql`（`messages.is_proactive` 加列）；只增不删 | — |

## 公共入口

| 模块 | 内容 |
|------|------|
| `database` | `init_pool` / `init_pool_with` / `PoolTuning`（migration 入口） |
| `repo` | 分实体 Repository：sessions / messages / l1 / events / facts / traits / keyword / jobs / settings 等 |
| `retry` | 写锁重试辅助 |
| 根级 | `SqliteStorage`（`StorageBackend` 的 SQLite 实现） |

## 相邻契约

- 依赖：`ramaria-core`（类型 / trait / 错误）。
- 被依赖：`ramaria-memory` / `ramaria-importer` / `ramaria-service`；入口（CLI / 桌面）经服务层使用，不直接依赖。
- **禁止**：LLM / 网络 / UI / 业务聚合逻辑。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增表 / 列 | `migrations/*.sql`（只增不删；破坏性变更须负责人授权） | `ramaria-core/src/types/` + `repo/*` + `../../../docs/architecture-ai-agent.md` |
| 新增/修改查询 | `src/repo/<entity>.rs` | `ramaria-core/src/traits/store_crud.rs` 的 trait 方法 + 各 mock 实现 |
| 连接池参数 | `src/database.rs`（`PoolTuning`） | 决策基线 §18 参数纪律 |
| 会话级联删除 | `src/repo/sessions.rs`（`delete_cascade`） | 服务层删除用例（CLI / 桌面共用） |

## 验证

```bash
cargo test -j 2 -p ramaria-storage
```
