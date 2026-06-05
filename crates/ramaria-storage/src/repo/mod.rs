//! rust/crates/ramaria-storage/src/repo/mod.rs - Repository 模块集合
//!
//! 设计特点:
//! - 每个子模块负责一类实体的 CRUD 操作
//! - Repository 函数接收 &SqlitePool，不持有状态
//! - 行映射使用手动 Row::get，避免 sqlx derive 侵入 core 层
//! - 错误统一返回 RamariaResult

pub mod backend_config;
pub mod background_jobs;
pub mod bm25_index;
pub mod graph;
pub mod memory_l1;
pub mod memory_l2;
pub mod messages;
pub mod privacy_consent;
pub mod schema_meta;
pub mod sessions;
pub mod user_profile;
