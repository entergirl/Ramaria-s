//! rust/crates/ramaria-storage/src/lib.rs - Ramaria SQLite 存储层
//!
//! 设计特点:
//! - 封装 SqlitePool，实现 `StorageBackend` trait 的所有业务 CRUD 方法
//! - Repository 模式：每个子模块负责一类实体的 SQL 操作与行映射
//! - 所有可恢复错误统一转换为 RamariaError::Storage
//! - 手动行映射避免 sqlx derive 侵入 core 层，保持零 I/O 约束
//! - 公共 API 与 `StorageBackend` trait 一致，供 app/memory 层依赖注入使用

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    BackendConfig, MemoryL1, MemoryL2, Message, PrivacyConsent, Session, UserProfile,
};
use sqlx::SqlitePool;
use uuid::Uuid;

pub mod database;
pub mod repo;

// =========================================================
// SqliteStorage - StorageBackend 实现
// =========================================================

/// SQLite 存储后端。
///
/// 职责:
/// - 持有 SqlitePool，负责所有数据持久化操作。
/// - 实现 StorageBackend trait，供 app 和 memory 层通过 trait object 注入。
///
/// 使用:
/// - `SqliteStorage::new(pool)` 创建实例。
/// - 所有方法委托给 `repo` 子模块的对应函数。
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    /// 创建新的 SqliteStorage 实例。
    ///
    /// 参数:
    /// - `pool`: 已初始化的 SqlitePool。
    ///
    /// 返回:
    /// - SqliteStorage 实例。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

// =========================================================
// StorageBackend trait 实现
// =========================================================

#[async_trait::async_trait]
impl StorageBackend for SqliteStorage {
    // -- Session --

    async fn create_session(&self) -> RamariaResult<Session> {
        let session = Session::new();
        repo::sessions::create_session(&self.pool, &session).await?;
        Ok(session)
    }

    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        repo::sessions::close_session(&self.pool, session_id).await
    }

    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        repo::sessions::get_session(&self.pool, session_id).await
    }

    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        repo::sessions::list_active_sessions(&self.pool).await
    }

    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        repo::sessions::list_sessions(&self.pool).await
    }

    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        repo::sessions::delete_session(&self.pool, session_id).await
    }

    // -- Message (L0) --

    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        repo::messages::save_message(&self.pool, message).await
    }

    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        repo::messages::list_messages(&self.pool, session_id).await
    }

    async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        repo::messages::find_message_by_fingerprint(&self.pool, fingerprint).await
    }

    // -- Memory L1 --

    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        repo::memory_l1::save_memory_l1(&self.pool, memory).await
    }

    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_memory_l1(&self.pool, session_id).await
    }

    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        repo::memory_l1::get_memory_l1(&self.pool, id).await
    }

    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()> {
        repo::memory_l1::mark_l1_absorbed(&self.pool, l1_ids).await
    }

    async fn list_unabsorbed_l1(&self) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_unabsorbed_l1(&self.pool).await
    }

    // -- Memory L2 --

    async fn save_memory_l2(&self, memory: &MemoryL2) -> RamariaResult<()> {
        repo::memory_l2::save_memory_l2(&self.pool, memory).await
    }

    async fn save_l2_sources(&self, l2_id: Uuid, l1_ids: &[Uuid]) -> RamariaResult<()> {
        repo::memory_l2::save_l2_sources(&self.pool, l2_id, l1_ids).await
    }

    async fn list_memory_l2(&self) -> RamariaResult<Vec<MemoryL2>> {
        repo::memory_l2::list_memory_l2(&self.pool).await
    }

    async fn get_l2_sources(&self, l2_id: Uuid) -> RamariaResult<Vec<Uuid>> {
        repo::memory_l2::get_l2_sources(&self.pool, l2_id).await
    }

    // -- User Profile (L3) --

    async fn save_user_profile(&self, profile: &UserProfile) -> RamariaResult<()> {
        repo::user_profile::save_user_profile(&self.pool, profile).await
    }

    async fn get_current_profile(&self) -> RamariaResult<Vec<UserProfile>> {
        repo::user_profile::get_current_profile(&self.pool).await
    }

    async fn mark_profile_historical(&self, field: &str) -> RamariaResult<()> {
        repo::user_profile::mark_profile_historical(&self.pool, field).await
    }

    // -- Privacy Consent --

    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        repo::privacy_consent::save_privacy_consent(&self.pool, consent).await
    }

    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        repo::privacy_consent::get_privacy_consent(&self.pool, provider, base_url).await
    }

    // -- Backend Config --

    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        repo::backend_config::save_backend_config(&self.pool, config).await
    }

    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        repo::backend_config::get_backend_config(&self.pool).await
    }

    // -- 索引一致性 --

    async fn get_schema_version(&self) -> RamariaResult<i32> {
        repo::schema_meta::get_schema_version(&self.pool).await
    }

    async fn get_index_version(&self) -> RamariaResult<i32> {
        repo::schema_meta::get_index_version(&self.pool).await
    }

    async fn set_index_version(&self, version: i32) -> RamariaResult<()> {
        repo::schema_meta::set_index_version(&self.pool, version).await
    }
}

// =========================================================
// 集成测试（StorageBackend trait 级别）
// =========================================================

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::database::test_pool;
    use ramaria_core::types::{MessageRole, MessageSource};

    /// 验证 SqliteStorage 可以作为 `dyn StorageBackend` 使用。
    #[tokio::test]
    async fn storage_backend_works_as_trait_object() {
        let pool = test_pool().await.unwrap();
        let storage: Box<dyn StorageBackend> = Box::new(SqliteStorage::new(pool));

        let session = storage.create_session().await.unwrap();
        assert!(session.is_active());
    }

    /// 验证完整的 send_message 闭环：session 创建 → 消息保存 → 查询 → 关闭。
    #[tokio::test]
    async fn full_send_message_cycle() {
        let pool = test_pool().await.unwrap();
        let storage = SqliteStorage::new(pool);

        // 1. 创建 session
        let session = storage.create_session().await.unwrap();

        // 2. 保存用户消息
        let user_msg = Message::new(
            session.id,
            MessageRole::User,
            "你好，今天天气如何？".into(),
            MessageSource::Local,
        );
        storage.save_message(&user_msg).await.unwrap();

        // 3. 保存助手回复
        let assistant_msg = Message::new(
            session.id,
            MessageRole::Assistant,
            "今天的天气很好！".into(),
            MessageSource::Local,
        );
        storage.save_message(&assistant_msg).await.unwrap();

        // 4. 查询消息
        let messages = storage.list_messages(session.id).await.unwrap();
        assert_eq!(messages.len(), 2);

        // 5. 关闭 session
        storage.close_session(session.id).await.unwrap();
        let closed = storage.get_session(session.id).await.unwrap().unwrap();
        assert!(!closed.is_active());
    }

    /// 验证 L0 → L1 → L2 记忆生命周期。
    #[tokio::test]
    async fn memory_lifecycle_cycle() {
        let pool = test_pool().await.unwrap();
        let storage = SqliteStorage::new(pool);

        // 创建 session + L1
        let session = storage.create_session().await.unwrap();
        let l1 = MemoryL1::new(session.id, "关于天气的对话摘要".into(), Some("下午".into()));
        storage.save_memory_l1(&l1).await.unwrap();

        // 确认 L1 存在
        let l1_list = storage.list_memory_l1(session.id).await.unwrap();
        assert_eq!(l1_list.len(), 1);

        // 创建 L2 + 溯源
        let now = ramaria_core::types::now_ms();
        let l2 = MemoryL2::new("周内对话聚合".into(), now - 604_800_000, now);
        storage.save_memory_l2(&l2).await.unwrap();
        storage.save_l2_sources(l2.id, &[l1.id]).await.unwrap();

        // 标记 L1 已吸收
        storage.mark_l1_absorbed(&[l1.id]).await.unwrap();

        // 验证 L2 溯源
        let sources = storage.get_l2_sources(l2.id).await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0], l1.id);
    }
}
