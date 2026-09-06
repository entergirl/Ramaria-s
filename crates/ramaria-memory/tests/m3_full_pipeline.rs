//! M3 全链路集成测试：mock LLM + fixture L1 → 事件提取 → motives/关系记录
//!
//! 验收要求：mock LLM + mock embedding + fixture L1 clusters → TopicBatcher →
//! ContextRetriever → 事件提取 → 验证：events 含 motives + event_relations 有记录
//! （≥3 种关系类型）。
//!
//! 链路（真实 SQLite + mock LLM）：
//!   内存库 + migrations → 创建 persona/session → 保存 50 条未吸收 L1（3 主题）
//!   → Retriever 索引（供 ContextRetriever 补充上下文）
//!   → EventExtractor::extract_events（TopicBatcher 聚类 → 每簇 LLM → 事件+关系入库）
//!   → 断言 events 含 motives + event_relations ≥ 3 种关系类型 + absorbed 标记。

mod common;

use common::{MockLlm, create_persona, create_session, make_l1, mem_storage};
use ramaria_core::traits::{StorageBackend, StoreCrud, StoreInfrastructure};
use ramaria_core::types::now_ms;
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::retriever::{L1DocView, Retriever};
use uuid::Uuid;

/// mock LLM 回复：2 个事件（含 motives）+ 3 种关系。
const MOCK_LLM_RESPONSE: &str = r#"{
  "events": [
    {
      "title": "项目延期讨论",
      "summary": "用户讨论项目延期安排，需求变更频繁。",
      "keywords": "工作,项目,延期",
      "participants": ["用户"],
      "confidence": 0.8,
      "salience": 0.7,
      "valence": -0.2,
      "presentation": "subjective",
      "share": 0.6,
      "attitude": null,
      "motives": ["责任压力", "时间管理"]
    },
    {
      "title": "跑步计划",
      "summary": "用户制定每周三次的晨跑计划。",
      "keywords": "健身,跑步,计划",
      "participants": ["用户"],
      "confidence": 0.7,
      "salience": 0.6,
      "valence": 0.5,
      "presentation": "objective",
      "share": 0.4,
      "attitude": null,
      "motives": ["自我提升"]
    },
    {
      "title": "餐厅推荐",
      "summary": "用户分享周末去过的餐厅。",
      "keywords": "美食,餐厅",
      "participants": ["用户"],
      "confidence": 0.9,
      "salience": 0.5,
      "valence": 0.6,
      "presentation": "subjective",
      "share": 0.7,
      "attitude": null,
      "motives": ["社交分享"]
    }
  ],
  "relations": [
    {"from_index": 0, "to_index": 1, "kind": "CausedBy", "weight": 0.6},
    {"from_index": 0, "to_index": 2, "kind": "PartOf", "weight": 0.5},
    {"from_index": 1, "to_index": 2, "kind": "Timeline", "weight": 0.4}
  ]
}"#;

/// 构造 3 主题 × 17 条 = 51 条未吸收 L1（跨主题关键词无交集）。
async fn seed_l1s(storage: &impl StorageBackend, session_id: Uuid, persona_uid: &str) -> Vec<Uuid> {
    let topics: [(&str, &[&str]); 3] = [
        ("工作", &["工作", "项目", "会议"]),
        ("健身", &["健身", "跑步", "运动"]),
        ("美食", &["美食", "餐厅", "做饭"]),
    ];
    let base = now_ms() - 30 * 24 * 3600 * 1000;
    let mut ids = Vec::new();
    let mut idx = 0i64;
    for (prefix, kws) in topics {
        for i in 0..17 {
            let l1 = make_l1(
                session_id,
                &format!("{prefix} 摘要{i}"),
                &kws.join(","),
                persona_uid,
                0.5 + (i as f64) * 0.01,
                base + idx * 60_000,
            );
            let id = l1.id;
            storage.save_memory_l1(&l1).await.expect("保存 L1 失败");
            ids.push(id);
            idx += 1;
        }
    }
    ids
}

/// 构建带关键词索引的 Retriever（供 ContextRetriever 补充上下文）。
fn build_retriever(persona_uid: &str, l1s: &[(Uuid, String, String)]) -> Retriever {
    let mut retriever = Retriever::new();
    for (id, summary, keywords) in l1s {
        retriever.index_l1(&L1DocView {
            id: *id,
            summary: summary.clone(),
            keywords: Some(keywords.clone()),
            persona_uid: Some(persona_uid.to_string()),
            created_at: now_ms(),
            salience: 0.6,
            last_accessed_at: None,
        });
    }
    retriever
}

/// 全链路验收主路径：50+ 条 L1 → 事件提取 → events 含 motives + relations ≥ 3 种。
#[tokio::test]
async fn full_pipeline_extracts_events_with_motives_and_relations() {
    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-e2e", "测试角色").await;
    let session = create_session(&storage, Some(&persona_uid)).await;
    let l1_ids = seed_l1s(&storage, session.id, &persona_uid).await;
    assert!(l1_ids.len() >= 50, "fixture 应 ≥ 50 条未吸收 L1");

    // 未吸收 L1 全部可见（触发条件 A：≥ 5）
    let unabsorbed = storage
        .list_unabsorbed_l1(&persona_uid)
        .await
        .expect("查询未吸收 L1 失败");
    assert_eq!(unabsorbed.len(), l1_ids.len());

    // Retriever 索引（提供补充上下文）
    let retriever = build_retriever(
        &persona_uid,
        &unabsorbed
            .iter()
            .map(|l| {
                (
                    l.id,
                    l.summary.clone(),
                    l.keywords.clone().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>(),
    );

    // EventExtractor + ContextRetriever
    let llm = MockLlm::new(MOCK_LLM_RESPONSE);
    let config = EventExtractorConfig {
        // 每簇最多 5 个事件，relations 索引引用 events 数组（2 条事件 + 3 条关系安全）
        max_events: 5,
        ..EventExtractorConfig::default()
    };
    let mut extractor = EventExtractor::new(&llm, &storage, config);
    extractor.set_retriever(&retriever);

    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("事件提取失败");

    // 1) 事件已提取且 motives 非空
    assert!(!events.is_empty(), "应提取到事件");
    for ev in &events {
        let motives = ev.motives.as_deref().unwrap_or("");
        assert!(
            !motives.trim().is_empty(),
            "事件 '{}' 应含 motives，实际: {:?}",
            ev.title,
            ev.motives
        );
    }

    // 2) event_relations 有记录且 ≥ 3 种关系类型
    let relations = storage
        .list_event_relations_by_persona(&persona_uid)
        .await
        .expect("查询事件关系失败");
    let kinds: std::collections::HashSet<String> = relations
        .iter()
        .map(|r| r.kind.as_str().to_string())
        .collect();
    assert!(
        kinds.len() >= 3,
        "应至少 3 种关系类型，实际 {} 种: {:?}",
        kinds.len(),
        kinds
    );
    for k in &kinds {
        assert_ne!(*k, "RelatedTo", "不应降级为默认 RelatedTo");
    }

    // 3) 事件溯源写入（event_sources）与 L1 absorbed 标记
    // 关系两端都应引用真实提取的事件 ID（FK→memory_events.id），而非 0/占位值
    let event_ids: std::collections::HashSet<i64> = events.iter().map(|e| e.id).collect();
    assert!(
        event_ids.len() == events.len() && !event_ids.contains(&0),
        "提取事件应携带非 0 的真实存储 ID"
    );
    for rel in &relations {
        assert!(
            event_ids.contains(&rel.from_id),
            "关系 from_id={} 应引用真实提取事件 ID",
            rel.from_id
        );
        assert!(
            event_ids.contains(&rel.to_id),
            "关系 to_id={} 应引用真实提取事件 ID",
            rel.to_id
        );
    }
    let remaining = storage
        .list_unabsorbed_l1(&persona_uid)
        .await
        .expect("查询未吸收 L1 失败");
    assert!(
        remaining.is_empty(),
        "全部 L1 应标记 absorbed，剩余 {} 条",
        remaining.len()
    );

    // 4) ContextRetriever 参与：LLM 最近请求 prompt 含补充上下文段落
    // 注：当前 ContextRetriever 无"排除当前簇"逻辑，簇内 L1 自身也会被召回；
    //     若未来增加簇排除优化，此断言需同步调整（依赖该耦合）。
    let last = llm.last_request().expect("应至少有一次 LLM 调用");
    assert!(
        last.user_message.contains("补充背景") || last.user_message.contains("仅供背景参考"),
        "prompt 应含补充上下文段落"
    );
}

/// 触发条件校验：未吸收 L1 < 5 时不提取（条件 A 不满足）。
#[tokio::test]
async fn trigger_not_fired_below_threshold() {
    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-thresh", "阈值角色").await;
    let session = create_session(&storage, Some(&persona_uid)).await;

    // 仅 2 条未吸收 L1（< trigger_count=5）
    for i in 0..2 {
        storage
            .save_memory_l1(&make_l1(
                session.id,
                &format!("少量摘要{i}"),
                "测试",
                &persona_uid,
                0.5,
                now_ms(),
            ))
            .await
            .unwrap();
    }

    let llm = MockLlm::new(MOCK_LLM_RESPONSE);
    let mut extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default());
    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("触发检查不应失败");
    assert!(events.is_empty(), "低于阈值不应触发提取");
}

/// 降级路径：LLM 返回非法 JSON → 生成降级事件（不阻塞管线）。
#[tokio::test]
async fn llm_failure_degrades_to_builtin_event() {
    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-degrade", "降级角色").await;
    let session = create_session(&storage, Some(&persona_uid)).await;

    for i in 0..6 {
        storage
            .save_memory_l1(&make_l1(
                session.id,
                &format!("降级摘要{i}"),
                "降级,测试",
                &persona_uid,
                0.5,
                now_ms() - 10_000 + i as i64 * 1000,
            ))
            .await
            .unwrap();
    }

    // 非法 JSON → parse 失败 → degrade_cluster
    let llm = MockLlm::new("这不是 JSON");
    let mut extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default());
    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("降级路径不应失败");
    assert!(!events.is_empty(), "降级应产出内置事件");
}

// =========================================================
// v1.5 L2 聚类去重指纹（三层生成缓存 C）
// =========================================================

/// 同集合跳过：L1 集合已聚类且无产出（无簇）→ 登记指纹 →
/// 再次提取同集合直接跳过（不重复聚类、不调用 LLM）。
#[tokio::test]
async fn l2_fingerprint_same_set_no_cluster_skips_second_extraction() {
    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-fp", "指纹角色").await;
    let session = create_session(&storage, Some(&persona_uid)).await;

    // 5 条关键词完全不同的未吸收 L1：满足触发阈值（≥5）但无法聚类成簇
    // （min_cluster_size=3，孤立点入 Pending Buffer 不成簇）→ "已聚类且无产出"。
    let topics = ["量子物理", "美食探店", "健身计划", "电影观感", "宠物养护"];
    for (i, topic) in topics.iter().enumerate() {
        storage
            .save_memory_l1(&make_l1(
                session.id,
                &format!("{topic}摘要{i}"),
                topic,
                &persona_uid,
                0.5,
                now_ms() - 60_000 + i as i64 * 1000,
            ))
            .await
            .unwrap();
    }

    let llm = MockLlm::new("{\"events\": []}");
    let mut extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default());

    // 第一次：聚类无簇产出 → 登记指纹，不应有任何 LLM 调用
    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("首次提取成功");
    assert!(events.is_empty());
    assert_eq!(llm.call_count(), 0, "无簇产出时不应调用 LLM");

    // 第二次：同集合 → 指纹命中 → 直接跳过（不重复聚类、不调 LLM）
    let calls_before = llm.call_count();
    let events2 = extractor
        .extract_events(&persona_uid)
        .await
        .expect("二次提取成功");
    assert!(events2.is_empty());
    assert_eq!(
        llm.call_count(),
        calls_before,
        "同集合应跳过：不重复聚类、不调用 LLM"
    );
}

/// 集合变更重聚类：同集合跳过之后新增 L1 → 集合指纹变化 →
/// 自动重新聚类（LLM 调用恢复，不被旧指纹误拦）。
#[tokio::test]
async fn l2_fingerprint_set_change_reclusters() {
    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-fp2", "指纹角色二").await;
    let session = create_session(&storage, Some(&persona_uid)).await;

    let topics = ["量子物理", "美食探店", "健身计划", "电影观感", "宠物养护"];
    for (i, topic) in topics.iter().enumerate() {
        storage
            .save_memory_l1(&make_l1(
                session.id,
                &format!("{topic}摘要{i}"),
                topic,
                &persona_uid,
                0.5,
                now_ms() - 60_000 + i as i64 * 1000,
            ))
            .await
            .unwrap();
    }

    let llm = MockLlm::new("{\"events\": []}");
    let mut extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default());

    // 第一次：无簇 → 登记指纹
    assert!(
        extractor
            .extract_events(&persona_uid)
            .await
            .unwrap()
            .is_empty()
    );
    // 第二次：同集合 → 跳过
    assert!(
        extractor
            .extract_events(&persona_uid)
            .await
            .unwrap()
            .is_empty()
    );
    let calls_after_skip = llm.call_count();
    assert_eq!(calls_after_skip, 0, "同集合跳过不应调用 LLM");

    // 集合变化：新增 1 条同主题 L1（可与前 5 条之一聚簇）→ 指纹变化 → 重新聚类
    storage
        .save_memory_l1(&make_l1(
            session.id,
            "量子物理摘要-新增",
            "量子物理",
            &persona_uid,
            0.5,
            now_ms(),
        ))
        .await
        .unwrap();

    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("集合变更后提取成功");
    // 新集合中"量子物理"关键词有 2 条（< min_cluster_size=3），仍可能无簇；
    // 关键断言是 LLM 被重新调用（指纹失效），而非事件产出。
    let _ = events;
    assert!(
        llm.call_count() > calls_after_skip,
        "集合变更应触发重新聚类（旧指纹不应误拦）"
    );
}

/// 相似度去重：新提取事件与 persona 最近已有事件近似重复（≥ 阈值）→
/// 跳过保存（不重复入库），且全部去重后登记指纹。
#[tokio::test]
async fn l2_fingerprint_similar_event_deduped() {
    use ramaria_core::types::MemoryEvent;

    let storage = mem_storage().await;
    let persona_uid = create_persona(&storage, "char-dedup", "去重角色").await;
    let session = create_session(&storage, Some(&persona_uid)).await;

    // 预置 1 条已有事件（模拟此前已入库的同内容事件）
    let mut existing = MemoryEvent::new(
        persona_uid.clone(),
        "项目延期".into(),
        "用户讨论项目延期安排，需求变更频繁。".into(),
        now_ms() - 1000,
        now_ms(),
    );
    existing.keywords = Some("工作,项目,延期".into());
    storage.save_event(&existing).await.unwrap();

    // 5 条同主题 L1 → 1 个簇 → LLM 返回与已有事件完全相同内容的事件
    for i in 0..5 {
        storage
            .save_memory_l1(&make_l1(
                session.id,
                &format!("项目延期摘要{i}"),
                "工作,项目,延期",
                &persona_uid,
                0.5,
                now_ms() - 60_000 + i as i64 * 1000,
            ))
            .await
            .unwrap();
    }

    // 与预置事件 title/summary/keywords 完全一致 → 相似度 ≈ 1.0 → 判重
    const DUP_LLM_RESPONSE: &str = r#"{
      "events": [
        {
          "title": "项目延期",
          "summary": "用户讨论项目延期安排，需求变更频繁。",
          "keywords": "工作,项目,延期",
          "confidence": 0.8,
          "salience": 0.7,
          "valence": -0.2,
          "presentation": "subjective",
          "share": 0.6,
          "attitude": null
        }
      ]
    }"#;
    let llm = MockLlm::new(DUP_LLM_RESPONSE);
    let mut extractor = EventExtractor::new(&llm, &storage, EventExtractorConfig::default());

    let events = extractor
        .extract_events(&persona_uid)
        .await
        .expect("提取成功");
    assert!(events.is_empty(), "近似重复事件应全部被去重跳过");
    assert_eq!(llm.call_count(), 1, "应恰好聚类 1 簇并调用 1 次 LLM");

    // 库中事件数不变（仍只有预置的 1 条），未产生重复入库
    let recent = storage
        .list_recent_events(&persona_uid, 10)
        .await
        .expect("查询最近事件成功");
    assert_eq!(recent.len(), 1, "相似事件不应重复入库");
    assert_eq!(recent[0].title, "项目延期");
}
