//! ramaria-memory 集成测试共享基础设施
//!
//! 提供:
//! - `MockLlm`: 实现 `LlmProviderTrait` 的 mock LLM（单点定义在 `mock_store`，本模块 re-export）
//! - `mem_storage`: 内存 SQLite（跑 v1.3 + v1.4 migration）→ `SqliteStorage`
//! - fixture 构造辅助：分组关键词 L1、mock embedding（预计算向量）

// 每个测试二进制独立编译本模块，各目标使用子集不同 → 允许未使用项
// （`unused_imports` 覆盖跨文件 re-export：部分测试二进制不使用全部导出）
#![allow(dead_code, unused_imports)]

pub mod mock_store;

pub use mock_store::MockLlm;

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MemoryL1, Persona, PersonaKind, Session, now_ms};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

// =========================================================
// 内存 SQLite 存储
// =========================================================

/// 创建内存 SQLite 存储（跑全部 migration）。
pub async fn mem_storage() -> SqliteStorage {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("内存测试数据库创建失败");
    sqlx::migrate!("../ramaria-storage/migrations")
        .run(&pool)
        .await
        .expect("测试 migration 失败");
    SqliteStorage::new(pool)
}

// =========================================================
// Fixture 构造
// =========================================================

/// 创建 persona 并返回 uid。
pub async fn create_persona(storage: &SqliteStorage, uid: &str, name: &str) -> String {
    let persona = Persona::new(
        uid.to_string(),
        name.to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("创建 persona 失败");
    uid.to_string()
}

/// 创建会话并返回 session。
pub async fn create_session(storage: &SqliteStorage, persona_uid: Option<&str>) -> Session {
    storage
        .create_session(persona_uid)
        .await
        .expect("创建 session 失败")
}

/// 构造一条未吸收 L1（默认 absorbed=false，persona_uid 关联）。
pub fn make_l1(
    session_id: Uuid,
    summary: &str,
    keywords: &str,
    persona_uid: &str,
    salience: f64,
    created_at: i64,
) -> MemoryL1 {
    let mut l1 = MemoryL1::new(session_id, summary.to_string(), Some("下午".to_string()));
    l1.keywords = Some(keywords.to_string());
    l1.persona_uid = Some(persona_uid.to_string());
    l1.salience = salience;
    l1.created_at = created_at;
    l1
}

/// 当前时间（Unix 毫秒）。
pub fn now() -> i64 {
    now_ms()
}
