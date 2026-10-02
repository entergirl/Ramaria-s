//! crates/ramaria-storage/src/tests/mod.rs - Ramaria 存储层测试模块
//!
//! 设计特点:
//! - 目录化拆分：会话 / 消息 / 人格 / L1 / 事件 / 事实 / 关键词 / 索引 / 后台任务 / 话语块 / 示例 / LLM 缓存按域分文件
//! - 本文件仅保留子模块声明与跨域共享夹具（setup / setup_with_persona），不含具体用例
//! - 统一以 init_test_pool 构建空库，用例间相互独立
//! - 通过 `use super::*` 保留 crate 内部访问能力（含 storage.pool 的直接访问）

use super::*;
use ramaria_core::config::CacheEviction;
use ramaria_core::error::RamariaError;
use ramaria_core::keyword::KeywordToken;
use ramaria_core::traits::{
    BM25_INDEX_VERSION_CURRENT, BM25_INDEX_VERSION_LEGACY, IndexCorpusStamp, LlmResponseCache,
    SETTING_BM25_INDEX_VERSION, StoreCrud, StoreInfrastructure,
};
use ramaria_core::types::{
    EventBatchWrite, EventRelation, EventRelationKind, EvidenceDirection, FactSource, MemoryEvent,
    MemoryL1, Message, MessageRole, MessageSource, Persona, PersonaFact, PersonaKind,
    PersonalityTrait, PrivacyConsent, TraitEvidence, TraitLayer, TraitSource, TraitStatus,
    UttBlock, now_ms,
};
use uuid::Uuid;

async fn setup() -> SqliteStorage {
    let pool = database::init_test_pool()
        .await
        .expect("测试数据库初始化失败");
    SqliteStorage::new(pool)
}

/// 辅助：创建含 persona 和 L1 的完整测试上下文。
async fn setup_with_persona() -> (SqliteStorage, String, i64, uuid::Uuid) {
    let storage = setup().await;
    let p = Persona::new(
        "user-test".into(),
        "测试角色".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    let persona_id = storage.create_persona(&p).await.unwrap();

    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "测试摘要".into(), Some("上午".into()));
    storage.save_memory_l1(&l1).await.unwrap();

    (storage, "user-test".to_string(), persona_id, l1.id)
}

mod background_job;
mod event;
mod example;
mod fact;
mod index;
mod keyword;
mod llm_cache;
mod memory_l1;
mod message;
mod persona;
mod personality_trait;
mod schema;
mod session;
mod utt_block;
