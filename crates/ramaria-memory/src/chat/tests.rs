//! crates/ramaria-memory/src/chat/tests.rs - 对话装配编排单元测试
//!
//! 设计特点:
//! - 覆盖脉络素材加载（闸门/加权/回退/懒加载槽）与脉络行格式化
//! - 使用真实 SQLite 临时库验证 L1 读写路径，不依赖真实 LLM/embedding
//! - 断言锁定脉络行格式与最后活跃时间口径

use super::narrative::search_result_to_memory_l1;
use super::*;
use std::sync::{Arc, RwLock};

use crate::bm25::DocId;
use crate::retriever::{L1DocView, Retriever, SearchResult};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryL1, Persona, PersonaKind};
use uuid::Uuid;

/// 用真实 SQLite（临时文件库）构造存储后端（脉络素材只需 L1 读写路径）。
async fn test_storage(tag: &str) -> Arc<dyn StorageBackend> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-chat-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
        .await
        .expect("测试库初始化应成功");
    Arc::new(ramaria_storage::SqliteStorage::new(pool))
}

/// 创建指定 uid 的 persona（满足 `memory_l1.persona_uid` 外键）。
async fn ensure_persona(storage: &dyn StorageBackend, persona_uid: &str) {
    let persona = Persona::new(
        persona_uid.to_string(),
        "测试人格".to_string(),
        PersonaKind::User,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("persona 创建应成功");
}

/// 写入一条指定 persona 的 L1（先建 persona 与 session 满足外键；created_at 为当前时间）。
async fn save_l1(storage: &dyn StorageBackend, persona_uid: &str, summary: &str) {
    ensure_persona(storage, persona_uid).await;
    let session = storage
        .create_session(None)
        .await
        .expect("session 创建应成功");
    let mut l1 = MemoryL1::new(session.id, summary.to_string(), Some("下午".to_string()));
    l1.persona_uid = Some(persona_uid.to_string());
    storage.save_memory_l1(&l1).await.expect("L1 写入应成功");
}

// =========================================================
// 脉络素材加载
// =========================================================

/// 闸门关闭（`injection.narrative=false`）→ 跳过 L1 加载，返回空素材。
#[tokio::test]
async fn gate_off_returns_empty_material() {
    let storage = test_storage("gate").await;
    // 预置 L1：若闸门不生效将被加载
    save_l1(storage.as_ref(), "rama-0001", "不应出现的摘要").await;

    let cfg = RamariaConfig::default();
    let retriever = RwLock::new(Retriever::new());
    let material = load_narrative_material(
        storage.as_ref(),
        &retriever,
        &cfg.retrieval,
        &cfg.decay,
        false,
        "rama-0001",
        "你好",
    )
    .await;

    assert!(
        material.recent_summaries.is_empty(),
        "闸门关闭应跳过 L1 摘要加载"
    );
    assert!(material.last_active_at.is_none(), "last_active_at 应为空");
}

/// 非加权路径：无条件取最近 N 条，最后活跃时间按 UTC `%Y-%m-%d %H:%M` 口径。
#[tokio::test]
async fn unweighted_path_reads_recent_l1_and_last_active() {
    let storage = test_storage("recent").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    let session = storage
        .create_session(None)
        .await
        .expect("session 创建应成功");
    // created_at 固定：2023-11-14 22:13:20 UTC
    let mut l1 = MemoryL1::new(
        session.id,
        "最近的一次对话摘要".to_string(),
        Some("下午".to_string()),
    );
    l1.atmosphere = Some("轻松".to_string());
    l1.persona_uid = Some("rama-0001".to_string());
    l1.created_at = 1_700_000_000_000;
    storage.save_memory_l1(&l1).await.expect("L1 写入应成功");

    let mut cfg = RamariaConfig::default();
    cfg.retrieval.narrative_weighted = false; // 回退"无条件取最近 N 条"
    let retriever = RwLock::new(Retriever::new());
    let material = load_narrative_material(
        storage.as_ref(),
        &retriever,
        &cfg.retrieval,
        &cfg.decay,
        true,
        "rama-0001",
        "完全不相关的话题",
    )
    .await;

    assert_eq!(material.recent_summaries.len(), 1);
    assert!(material.recent_summaries[0].contains("最近的一次对话摘要"));
    assert!(material.recent_summaries[0].contains("下午"));
    assert!(material.recent_summaries[0].contains("轻松"));
    assert_eq!(
        material.last_active_at.as_deref(),
        Some("2023-11-14 22:13"),
        "最后活跃时间应按 UTC %Y-%m-%d %H:%M 格式化"
    );
}

/// 加权路径（默认）：以当前消息为话题依据，话题相关的 L1 优先注入。
#[tokio::test]
async fn weighted_path_ranks_topic_relevant_first() {
    let storage = test_storage("weighted").await;
    let cfg = RamariaConfig::default(); // narrative_weighted = true
    let now = ramaria_core::types::now_ms();

    let mut retriever = Retriever::new();
    retriever.index_l1(&L1DocView {
        id: Uuid::new_v4(),
        summary: "用户讨论了Rust异步编程".to_string(),
        keywords: Some("Rust,编程".to_string()),
        persona_uid: Some("rama-0001".to_string()),
        created_at: now - 3 * 86_400_000,
        salience: 0.5,
        last_accessed_at: None,
    });
    retriever.index_l1(&L1DocView {
        id: Uuid::new_v4(),
        summary: "用户和朋友去吃了火锅".to_string(),
        keywords: Some("社交,火锅".to_string()),
        persona_uid: Some("rama-0001".to_string()),
        created_at: now - 86_400_000,
        salience: 0.5,
        last_accessed_at: None,
    });
    let retriever = RwLock::new(retriever);

    let material = load_narrative_material(
        storage.as_ref(),
        &retriever,
        &cfg.retrieval,
        &cfg.decay,
        true,
        "rama-0001",
        "Rust 编程",
    )
    .await;

    assert!(
        !material.recent_summaries.is_empty(),
        "加权注入应有脉络结果"
    );
    assert!(
        material.recent_summaries[0].contains("Rust"),
        "话题相关的 L1 应优先注入，got: {:?}",
        material.recent_summaries[0]
    );
}

/// 加权检索无结果（空索引）→ 回退最近 N 条（不丢脉络）。
#[tokio::test]
async fn weighted_no_hit_falls_back_to_recent_l1() {
    let storage = test_storage("weighted-fallback").await;
    save_l1(storage.as_ref(), "rama-0001", "回退取到的最近摘要").await;

    let cfg = RamariaConfig::default(); // narrative_weighted = true
    let retriever = RwLock::new(Retriever::new()); // 空索引 → 检索无结果
    let material = load_narrative_material(
        storage.as_ref(),
        &retriever,
        &cfg.retrieval,
        &cfg.decay,
        true,
        "rama-0001",
        "任意输入",
    )
    .await;

    assert_eq!(material.recent_summaries.len(), 1);
    assert!(material.recent_summaries[0].contains("回退取到的最近摘要"));
}

/// 服务层懒加载槽（`RwLock<Option<Retriever>>`）未加载 → 空检索 → 回退最近 N 条。
#[tokio::test]
async fn unloaded_slot_falls_back_to_recent_l1() {
    let storage = test_storage("slot").await;
    save_l1(storage.as_ref(), "rama-0001", "懒加载槽未加载时回退摘要").await;

    let cfg = RamariaConfig::default();
    let slot: RwLock<Option<Retriever>> = RwLock::new(None);
    let material = load_narrative_material(
        storage.as_ref(),
        &slot,
        &cfg.retrieval,
        &cfg.decay,
        true,
        "rama-0001",
        "任意输入",
    )
    .await;

    assert_eq!(material.recent_summaries.len(), 1);
    assert!(material.recent_summaries[0].contains("懒加载槽未加载时回退摘要"));
}

// =========================================================
// 脉络行格式化与结果转换
// =========================================================

#[test]
fn format_line_with_time_and_atmosphere() {
    let mut l1 = MemoryL1::new(
        Uuid::new_v4(),
        "讨论了编程".to_string(),
        Some("下午".to_string()),
    );
    l1.atmosphere = Some("轻松".to_string());
    assert_eq!(
        format_l1_as_context_line(&l1),
        "下午 — 讨论了编程。氛围轻松。"
    );
}

#[test]
fn format_line_with_time_only() {
    let l1 = MemoryL1::new(
        Uuid::new_v4(),
        "讨论了编程".to_string(),
        Some("上午".to_string()),
    );
    assert_eq!(format_l1_as_context_line(&l1), "上午 — 讨论了编程");
}

#[test]
fn format_line_with_atmosphere_only() {
    let mut l1 = MemoryL1::new(Uuid::new_v4(), "讨论了编程".to_string(), None);
    l1.atmosphere = Some("融洽".to_string());
    assert_eq!(format_l1_as_context_line(&l1), "讨论了编程。氛围融洽。");
}

#[test]
fn format_line_without_annotations() {
    let l1 = MemoryL1::new(Uuid::new_v4(), "讨论了编程".to_string(), None);
    assert_eq!(format_l1_as_context_line(&l1), "讨论了编程");
}

/// 单条摘要截断到 120 字符（预算内含省略号）。
#[test]
fn format_line_truncates_to_120_chars() {
    let long_summary = "这是一段非常长的摘要".repeat(20);
    let l1 = MemoryL1::new(Uuid::new_v4(), long_summary, None);
    let formatted = format_l1_as_context_line(&l1);
    assert_eq!(
        formatted.chars().count(),
        120,
        "截断结果应为预算内 120 字符"
    );
    assert!(formatted.ends_with('…'));
}

/// 加权结果转换：仅 L1 层可转（L2/图谱不是脉络注入目标），标注字段置空。
#[test]
fn search_result_conversion_rules() {
    let l1_id = Uuid::new_v4();
    let l1_sr = SearchResult {
        doc_id: DocId::L1(l1_id),
        layer: "l1".to_string(),
        rrf_score: 0.8,
        bm25_score: Some(0.8),
        vector_score: None,
        graph_score: None,
        persona_uid: Some("rama-0001".to_string()),
        share: None,
        created_at: 1_700_000_000_000,
        last_accessed_at: None,
        doc_summary: "用户讨论了Rust".to_string(),
    };
    let l1 = search_result_to_memory_l1(&l1_sr).expect("L1 结果应转换成功");
    assert_eq!(l1.id, l1_id);
    assert_eq!(l1.summary, "用户讨论了Rust");
    assert_eq!(l1.created_at, 1_700_000_000_000);
    assert_eq!(l1.session_id, Uuid::nil(), "脉络行不参与会话归属");
    assert!(l1.time_period.is_none());
    assert!(l1.atmosphere.is_none());

    let l2_sr = SearchResult {
        doc_id: DocId::L2(42),
        layer: "l2".to_string(),
        rrf_score: 1.0,
        bm25_score: None,
        vector_score: None,
        graph_score: None,
        persona_uid: Some("rama-0001".to_string()),
        share: None,
        created_at: 1_000,
        last_accessed_at: None,
        doc_summary: "事件摘要".to_string(),
    };
    assert!(
        search_result_to_memory_l1(&l2_sr).is_none(),
        "非 L1 层不应进入脉络注入"
    );
}
