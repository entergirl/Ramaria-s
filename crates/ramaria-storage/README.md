# ramaria-storage

> 定位：SQLite 存储层——schema（27 张表基线 + 增量迁移）、Repository 模式 CRUD、索引存取、重试。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§4 数据与存储）；表结构与接口现状见 `../../../docs/architecture-ai-agent.md`。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿）。

## 职责

- **migration 管理**：`database::init_pool` 建池并执行 `migrations/` 全量迁移；WAL、外键、busy_timeout（多进程共库等待写锁而非失败）。
- **Repository 实现**：每个实体一个 `repo/*.rs`，负责 SQL 与行映射；`SqliteStorage` 聚合实现 `StoreCrud` + `StoreInfrastructure`。
- **事务与一致性**：事件批量写入（`save_event_batch`）、会话级联删除（`delete_session_cascade`）等单事务语义。
- **索引与缓存存取**：BM25 索引、关键词池/引用、LLM 响应缓存、后台任务（`background_jobs`）。
- **写锁重试**：`retry` 模块提供多进程/多线程共库下的写冲突重试辅助。

## 公共入口

| 模块 | 内容 |
|------|------|
| `database` | `init_pool` / `init_pool_with` / `PoolTuning`（migration 入口） |
| `repo` | 分实体 Repository：sessions / messages / l1 / events / facts / traits / keyword / jobs / settings 等 |
| `retry` | 写锁重试辅助 |
| 根级 | `SqliteStorage`（`StorageBackend` 的 SQLite 实现） |

## 相邻契约

- 依赖：`ramaria-core`（类型 / trait / 错误）。
- 被依赖：`ramaria-memory`、`ramaria-service`、入口层（经 service 或不直接依赖）。
- **禁止**：LLM / 网络 / UI / 业务聚合逻辑。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增表 / 列 | `migrations/*.sql`（只增不删；破坏性变更须负责人授权） | `ramaria-core/src/types.rs` + `repo/*` + `../../../docs/architecture-ai-agent.md` |
| 新增/修改查询 | `src/repo/<entity>.rs` | `ramaria-core/src/traits.rs` 的 trait 方法 + 各 mock 实现 |
| 连接池参数 | `src/database.rs`（`PoolTuning`） | 决策基线 §18 参数纪律 |
| 会话级联删除 | `src/repo/sessions.rs`（`delete_cascade`） | 桌面 `delete_session` command 行为 |

## 验证

```bash
cargo test -j 2 -p ramaria-storage
```
