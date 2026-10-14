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

/// last_active_at 取最近 L1 的发生时间：登记为导入时刻、发生为历史时刻 → 显示发生时间。
#[tokio::test]
async fn last_active_at_uses_occurred_at() {
    let storage = test_storage("occurred").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    let session = storage
        .create_session(None)
        .await
        .expect("session 创建应成功");
    // 登记时间 = 现在（模拟导入时刻）；发生时间 = 2023-11-14 22:13:20 UTC
    let mut l1 = MemoryL1::new(
        session.id,
        "导入的历史摘要".to_string(),
        Some("下午".to_string()),
    );
    l1.persona_uid = Some("rama-0001".to_string());
    l1.created_at = ramaria_core::types::now_ms();
    l1.occurred_at = Some(1_700_000_000_000);
    storage.save_memory_l1(&l1).await.expect("L1 写入应成功");

    let mut cfg = RamariaConfig::default();
    cfg.retrieval.narrative_weighted = false;
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

    assert_eq!(
        material.last_active_at.as_deref(),
        Some("2023-11-14 22:13"),
        "最后活跃时间应取发生时间（而非导入时刻）"
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
        occurred_at: now - 3 * 86_400_000,
        salience: 0.5,
        last_accessed_at: None,
    });
    retriever.index_l1(&L1DocView {
        id: Uuid::new_v4(),
        summary: "用户和朋友去吃了火锅".to_string(),
        keywords: Some("社交,火锅".to_string()),
        persona_uid: Some("rama-0001".to_string()),
        created_at: now - 86_400_000,
        occurred_at: now - 86_400_000,
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

// =========================================================
// 分段格式声明（按通道）
// =========================================================

/// 组装最小分段格式输入（其余素材字段取空集）。
fn segment_style_inputs<'a>(
    summaries: &'a [String],
    labels: &'a [String],
    injection: &'a ramaria_core::config::InjectionGate,
    layer_dedup: &'a ramaria_core::config::LayerDedupConfig,
    style: SegmentStyle,
    group_conversation: bool,
) -> PromptMaterialInputs<'a> {
    PromptMaterialInputs {
        persona_uid: Some("rama-0001"),
        recent_summaries: summaries,
        last_active_at: None,
        utt_context: None,
        bridge_context: None,
        behavior_decision: None,
        examples: Vec::new(),
        max_examples: 3,
        knowledge_facts: Vec::new(),
        rag_covered_labels: labels,
        knowledge_budget_chars: None,
        injection,
        style_enabled: false,
        rag_text: None,
        layer_dedup,
        segment_style: style,
        group_conversation,
        recent_state: RecentStateConfig::default(),
        proactive: None,
    }
}

/// 通道（Newline）：核心规则后追加分条格式声明段（契约回到 `||`，平台逐条发送）。
#[tokio::test]
async fn newline_segment_style_appends_channel_declaration() {
    let storage = test_storage("segment-newline").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();

    let prompt = build_system_prompt(
        storage.as_ref(),
        &segment_style_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            SegmentStyle::Newline,
            false,
        ),
    )
    .await;

    assert!(
        prompt.contains(CHANNEL_SEGMENT_DECLARATION),
        "通道声明段应进入提示词"
    );
    assert!(prompt.contains("「||」"), "声明应要求用 || 分条");
    assert!(prompt.contains("逐条"), "声明应说明平台逐条分别发送");
    assert!(
        prompt.contains("覆盖上文的分条约定"),
        "声明应以格式要求覆盖前述约定"
    );
}

/// 通道（Newline + 群聊）：分段声明后追加群聊说明段；非群聊不追加。
#[tokio::test]
async fn group_declaration_appended_only_for_group_conversation() {
    let storage = test_storage("segment-group").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();

    let group_prompt = build_system_prompt(
        storage.as_ref(),
        &segment_style_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            SegmentStyle::Newline,
            true,
        ),
    )
    .await;
    assert!(
        group_prompt.contains(CHANNEL_GROUP_DECLARATION),
        "群聊形态应追加群聊说明段"
    );
    let segment_pos = group_prompt
        .find(CHANNEL_SEGMENT_DECLARATION)
        .expect("群聊提示词应含通道分段声明");
    let group_pos = group_prompt
        .find(CHANNEL_GROUP_DECLARATION)
        .expect("群聊提示词应含群聊说明段");
    assert!(segment_pos < group_pos, "群聊说明段应紧跟分段声明之后");

    let private_prompt = build_system_prompt(
        storage.as_ref(),
        &segment_style_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            SegmentStyle::Newline,
            false,
        ),
    )
    .await;
    assert!(
        !private_prompt.contains(CHANNEL_GROUP_DECLARATION),
        "非群聊不应包含群聊说明段"
    );
}

/// 管道（Pipe）：与现状一致，不追加通道声明与群聊说明段。
#[tokio::test]
async fn pipe_segment_style_keeps_legacy_rules() {
    let storage = test_storage("segment-pipe").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();

    let prompt = build_system_prompt(
        storage.as_ref(),
        &segment_style_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            SegmentStyle::Pipe,
            false,
        ),
    )
    .await;

    assert!(
        !prompt.contains(CHANNEL_SEGMENT_DECLARATION),
        "管道形态不应包含通道声明"
    );
    assert!(
        !prompt.contains(CHANNEL_GROUP_DECLARATION),
        "管道形态不应包含群聊说明段"
    );
    assert!(prompt.contains("||"), "共享回复规则仍含多气泡契约");
}

// =========================================================
// 近期状态注入
// =========================================================

/// 写入一条带效价 / 显著性的 L1（发生时间为当前时间，窗口内）。
async fn save_l1_with_valence(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    valence: f64,
    salience: f64,
) {
    let session = storage
        .create_session(None)
        .await
        .expect("session 创建应成功");
    let mut l1 = MemoryL1::new(session.id, "摘要".to_string(), Some("下午".to_string()));
    l1.persona_uid = Some(persona_uid.to_string());
    l1.valence = valence;
    l1.salience = salience;
    storage.save_memory_l1(&l1).await.expect("L1 写入应成功");
}

/// 在最小装配输入上覆盖近期状态配置。
fn recent_state_inputs<'a>(
    summaries: &'a [String],
    labels: &'a [String],
    injection: &'a ramaria_core::config::InjectionGate,
    layer_dedup: &'a ramaria_core::config::LayerDedupConfig,
    recent_state: RecentStateConfig,
) -> PromptMaterialInputs<'a> {
    let mut inputs = segment_style_inputs(
        summaries,
        labels,
        injection,
        layer_dedup,
        SegmentStyle::Pipe,
        false,
    );
    inputs.recent_state = recent_state;
    inputs
}

/// 近期状态开启：窗口内正向效价为主 → 提示词含轻快语气行。
#[tokio::test]
async fn recent_state_positive_mood_injected_when_enabled() {
    let storage = test_storage("recent-mood-positive").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    // 两条高显著正向 + 一条低显著负向 → 加权效价明显正向
    save_l1_with_valence(storage.as_ref(), "rama-0001", 0.9, 1.0).await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", 0.8, 1.0).await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", -0.6, 0.1).await;

    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();
    let prompt = build_system_prompt(
        storage.as_ref(),
        &recent_state_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            RecentStateConfig {
                enabled: true,
                window_days: 7,
            },
        ),
    )
    .await;

    assert!(
        prompt.contains("近期状态：最近心情不错，语气可以轻快一些。"),
        "开启且方向明确时应注入近期状态行"
    );
}

/// 近期状态开启：负向效价为主 → 提示词含放缓语气行。
#[tokio::test]
async fn recent_state_negative_mood_injected_when_enabled() {
    let storage = test_storage("recent-mood-negative").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", -0.9, 1.0).await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", -0.7, 1.0).await;

    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();
    let prompt = build_system_prompt(
        storage.as_ref(),
        &recent_state_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            RecentStateConfig {
                enabled: true,
                window_days: 7,
            },
        ),
    )
    .await;

    assert!(
        prompt.contains("近期状态：最近心情有些低落，语气放缓一些，别太跳脱。"),
        "负向主导时应注入放缓语气行"
    );
}

/// 开关关闭：窗口内效价明确也不查询、不注入（消费可关）。
#[tokio::test]
async fn recent_state_disabled_skips_injection() {
    let storage = test_storage("recent-mood-off").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", 0.9, 1.0).await;
    save_l1_with_valence(storage.as_ref(), "rama-0001", 0.8, 1.0).await;

    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();
    let prompt = build_system_prompt(
        storage.as_ref(),
        &recent_state_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            RecentStateConfig::default(),
        ),
    )
    .await;

    assert!(!prompt.contains("近期状态"), "关闭时不应注入近期状态行");
}

/// 无样本：开启但窗口内无 L1 → 不含近期状态行。
#[tokio::test]
async fn recent_state_without_samples_skips_injection() {
    let storage = test_storage("recent-mood-empty").await;
    ensure_persona(storage.as_ref(), "rama-0001").await;

    let config = RamariaConfig::default();
    let summaries: Vec<String> = Vec::new();
    let labels: Vec<String> = Vec::new();
    let prompt = build_system_prompt(
        storage.as_ref(),
        &recent_state_inputs(
            &summaries,
            &labels,
            &config.injection,
            &config.layer_dedup,
            RecentStateConfig {
                enabled: true,
                window_days: 7,
            },
        ),
    )
    .await;

    assert!(!prompt.contains("近期状态"), "无样本时不应注入近期状态行");
}
