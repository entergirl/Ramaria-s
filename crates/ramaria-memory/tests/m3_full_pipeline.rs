//! T-V13-3-010：M3 全链路集成测试（v1.3 遗留收尾补齐，T-V14-8-001）
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
use ramaria_core::traits::StorageBackend;
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
    let sources_ok = relations.iter().all(|r| r.from_id > 0 && r.to_id > 0);
    assert!(sources_ok, "关系应引用真实事件 ID");
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
