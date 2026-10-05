//! crates/ramaria-service/src/browse/tests.rs - Ramaria 记忆与会话浏览用例单元测试
//!
//! 设计特点:
//! - 由 browse 模块以 `#[cfg(test)] mod tests;` 收纳：覆盖 L1 / L2 / L3 / 三层画像 /
//!   画像状态 / 事实 / 证据链 / 会话浏览 / 通道概览 / 未读标记与汇总十组路径
//! - 使用真实 SQLite 临时库（全量 migration）：断言以浏览视图结构与落库状态为准
//! - 分页与钳制边界逐项锁定（offset / limit / has_more / 消息计数按 0 降级）
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use crate::test_support::{
    engine_with_db, seed_channel_session, seed_l1, seed_messages, seed_persona,
    seed_session_with_messages,
};
use crate::types::{
    FactBrowseRequest, L1BrowseRequest, L2BrowseRequest, SessionBrowseRequest,
    SessionMessagesRequest, TraitEvidenceRequest,
};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    EvidenceDirection, EvidenceNote, FactSource, MemoryEvent, MemoryL1, Message, MessageRole,
    MessageSource, PersonaFact, PersonalityTrait, ProfileField, TraitEvidence, TraitLayer,
    TraitSource, TraitStatus,
};
use ramaria_storage::SqliteStorage;
use std::time::Duration;
use uuid::Uuid;

/// 造一条事件（start / created_at 取同一时间戳，便于排序断言）。
async fn seed_event(storage: &SqliteStorage, persona: &str, title: &str, created_at: i64) -> i64 {
    let mut ev = MemoryEvent::new(
        persona.to_string(),
        title.to_string(),
        format!("{title}的摘要"),
        created_at,
        created_at + 1_000,
    );
    ev.created_at = created_at;
    storage.save_event(&ev).await.expect("写入事件应成功")
}

/// 造一条性格标签（可指定分层 / 层内序号 / 有效证据量）。
async fn seed_trait(
    storage: &SqliteStorage,
    persona: &str,
    label: &str,
    layer: TraitLayer,
    seq: i32,
    evidence: f64,
) -> i64 {
    let mut t = PersonalityTrait::new(
        persona.to_string(),
        layer,
        label.to_string(),
        format!("{label}的具体含义"),
        TraitSource::Manual,
        seq,
    );
    t.evidence = evidence;
    storage.save_trait(&t).await.expect("写入性格标签应成功")
}

/// 造一条知识事实（active，返回 id）。
async fn seed_fact(
    storage: &SqliteStorage,
    persona: &str,
    field: ProfileField,
    content: &str,
) -> i64 {
    let f = PersonaFact::new(
        persona.to_string(),
        field,
        content.to_string(),
        FactSource::Manual,
    );
    storage.save_fact(&f).await.expect("写入事实应成功")
}

/// L1 桌面口径：按会话收集 + persona 过滤 + 创建时间倒序 + limit 截断。
#[tokio::test]
async fn l1_desktop_scope_filters_orders_and_truncates() {
    let (engine, storage, dir) = engine_with_db("browse-l1-desktop").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_l1(&storage, "char-0001", "摘要 A", Some("考试"), 1_000).await;
    seed_l1(&storage, "char-0002", "摘要 B", None, 2_000).await;
    seed_l1(&storage, "char-0001", "摘要 C", None, 3_000).await;

    // persona 过滤：只含 char-0001 的两条，创建时间倒序
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: false,
            limit: None,
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(page.total, 2);
    let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 C", "摘要 A"], "应按创建时间倒序");
    assert!(
        page.items
            .iter()
            .all(|m| m.persona_uid.as_deref() == Some("char-0001"))
    );
    let a_item = page
        .items
        .iter()
        .find(|m| m.summary == "摘要 A")
        .expect("摘要 A 应出现");
    assert_eq!(a_item.keywords.as_deref(), Some("考试"), "伴随字段应透传");

    // 无 persona：三条全含
    let all = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: false,
            limit: None,
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(all.total, 3);
    let summaries: Vec<&str> = all.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 C", "摘要 B", "摘要 A"]);

    // limit 截断：取最新一条
    let limited = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: false,
            limit: Some(1),
            offset: None,
        })
        .await
        .expect("L1 浏览应成功");
    assert_eq!(limited.items.len(), 1);
    assert_eq!(limited.items[0].summary, "摘要 C");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L1 未吸收口径：分页 + total 为分页前条数；persona 缺省报业务校验错误。
#[tokio::test]
async fn l1_unabsorbed_scope_paginates() {
    let (engine, storage, dir) = engine_with_db("browse-l1-unabsorbed").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "摘要 1", None, 1_000).await;
    let absorbed = seed_l1(&storage, "char-0001", "摘要 2", None, 2_000).await;
    seed_l1(&storage, "char-0001", "摘要 3", None, 3_000).await;
    storage
        .mark_l1_absorbed(&[absorbed])
        .await
        .expect("标记吸收应成功");

    // 全量：未吸收 2 条（按创建时间升序取回）
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: true,
            limit: None,
            offset: None,
        })
        .await
        .expect("未吸收浏览应成功");
    assert_eq!(page.total, 2, "total 为分页前条数");
    let summaries: Vec<&str> = page.items.iter().map(|m| m.summary.as_str()).collect();
    assert_eq!(summaries, vec!["摘要 1", "摘要 3"]);

    // 分页：offset 1 + limit 1 → 第二条
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some("char-0001".to_string()),
            unabsorbed_only: true,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("未吸收浏览应成功");
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].summary, "摘要 3");

    // persona 缺省：业务校验错误
    let err = engine
        .memory_l1(L1BrowseRequest {
            persona: None,
            unabsorbed_only: true,
            limit: None,
            offset: None,
        })
        .await
        .expect_err("未吸收口径需 persona");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L2 persona 口径：分页 + total 为全量计数。
#[tokio::test]
async fn l2_persona_scope_paginates_with_total() {
    let (engine, storage, dir) = engine_with_db("browse-l2-persona").await;
    seed_persona(&storage, "char-0001").await;
    for i in 0..5_i64 {
        seed_event(&storage, "char-0001", &format!("事件{i}"), 1_000 + i * 10).await;
    }

    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: Some("char-0001".to_string()),
            limit: Some(2),
            offset: Some(1),
        })
        .await
        .expect("L2 浏览应成功");
    assert_eq!(page.total, 5, "total 为分页前全量计数");
    assert_eq!(page.items.len(), 2);
    // 按 start DESC：offset 1 跳过最新一条
    assert_eq!(page.items[0].title, "事件3");
    assert_eq!(page.items[1].title, "事件2");
    assert_eq!(page.items[0].persona_uid, "char-0001");
    // 事件起止时间随视图透出（与底层事件逐项一致）
    assert_eq!(page.items[0].start, 1_030, "start 应取自事件开始时间");
    assert_eq!(page.items[0].end, 2_030, "end 应取自事件结束时间");
    for item in &page.items {
        let event = storage
            .get_event(item.id)
            .await
            .expect("查询事件应成功")
            .expect("事件应存在");
        assert_eq!(item.start, event.start, "start 应与底层事件一致");
        assert_eq!(item.end, event.end, "end 应与底层事件一致");
        assert_eq!(item.created_at, event.created_at, "created_at 口径不变");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// L2 合并口径：逐 persona 取回后合并倒序截断，total 为合并条数。
#[tokio::test]
async fn l2_all_personas_merges_and_truncates() {
    let (engine, storage, dir) = engine_with_db("browse-l2-merge").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_event(&storage, "char-0001", "一A", 1_000).await;
    seed_event(&storage, "char-0001", "一B", 2_000).await;
    seed_event(&storage, "char-0002", "二A", 3_000).await;
    seed_event(&storage, "char-0002", "二B", 4_000).await;

    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: None,
            limit: Some(3),
            offset: None,
        })
        .await
        .expect("L2 合并浏览应成功");
    assert_eq!(page.total, 4, "total 为合并后（截断前）条数");
    let titles: Vec<&str> = page.items.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["二B", "二A", "一B"], "应按创建时间倒序截断");
    assert_eq!(page.items[0].start, 4_000, "合并口径同样透出事件起止时间");

    let _ = std::fs::remove_dir_all(&dir);
}

/// L3 两分支：persona 过滤与全人格合并。
#[tokio::test]
async fn l3_both_scopes() {
    let (engine, storage, dir) = engine_with_db("browse-l3").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    seed_trait(&storage, "char-0001", "温和", TraitLayer::Base, 1, 2.0).await;
    seed_trait(&storage, "char-0001", "幽默", TraitLayer::Primary, 1, 1.0).await;
    seed_trait(&storage, "char-0002", "直率", TraitLayer::Base, 1, 1.0).await;

    let scoped = engine
        .memory_l3(Some("char-0001"))
        .await
        .expect("L3 浏览应成功");
    assert_eq!(scoped.len(), 2);
    assert!(scoped.iter().all(|t| t.persona_uid == "char-0001"));

    let merged = engine.memory_l3(None).await.expect("L3 浏览应成功");
    assert_eq!(merged.len(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 三层画像：分组 / 层内 seq 排序 / 生效过滤 / 人格不存在报错。
#[tokio::test]
async fn personality_profile_groups_and_filters() {
    let (engine, storage, dir) = engine_with_db("browse-profile").await;
    seed_persona(&storage, "char-0001").await;
    seed_trait(&storage, "char-0001", "底色二", TraitLayer::Base, 2, 1.0).await;
    seed_trait(&storage, "char-0001", "底色一", TraitLayer::Base, 1, 1.0).await;
    seed_trait(&storage, "char-0001", "主色", TraitLayer::Primary, 1, 1.0).await;
    seed_trait(&storage, "char-0001", "点缀", TraitLayer::Accent, 1, 1.0).await;
    // 非生效标签不参与展示
    let deprecated = seed_trait(&storage, "char-0001", "旧标签", TraitLayer::Base, 3, 1.0).await;
    storage
        .update_trait_status(deprecated, TraitStatus::Deprecated)
        .await
        .expect("更新状态应成功");

    let profile = engine
        .personality_profile("char-0001")
        .await
        .expect("三层画像应成功");
    assert_eq!(profile.persona_uid, "char-0001");
    assert_eq!(profile.base.len(), 2);
    assert_eq!(profile.primary.len(), 1);
    assert_eq!(profile.accent.len(), 1);
    let base_labels: Vec<&str> = profile.base.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(base_labels, vec!["底色一", "底色二"], "层内按 seq 升序");

    // 人格不存在 / 空 uid：业务校验错误
    let err = engine
        .personality_profile("char-9999")
        .await
        .expect_err("人格不存在应报错");
    assert_eq!(err.category(), "validation");
    let err = engine
        .personality_profile("  ")
        .await
        .expect_err("空 uid 应报错");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 画像数据状态：三档阈值与描述文本。
#[tokio::test]
async fn profile_status_thresholds() {
    let (engine, storage, dir) = engine_with_db("browse-status").await;
    seed_persona(&storage, "char-0001").await;
    let t1 = seed_trait(&storage, "char-0001", "标签一", TraitLayer::Base, 1, 3.0).await;
    let t2 = seed_trait(&storage, "char-0001", "标签二", TraitLayer::Base, 2, 0.0).await;

    // 3.0 → insufficient
    let status = engine
        .profile_status("char-0001")
        .await
        .expect("状态读取应成功");
    assert_eq!(status.status, "insufficient");
    assert_eq!(status.active_trait_count, 2);
    assert!((status.n_total_eff - 3.0).abs() < f64::EPSILON);
    assert!(
        status.status_text.contains("数据不足"),
        "文案: {}",
        status.status_text
    );

    // 3.0 + 10.0 = 13.0 → preliminary
    storage
        .update_trait_confidence(t1, 0.8, 13.0, 0.5)
        .await
        .expect("更新证据量应成功");
    let status = engine
        .profile_status("char-0001")
        .await
        .expect("状态读取应成功");
    assert_eq!(status.status, "preliminary");
    assert!((status.n_total_eff - 13.0).abs() < f64::EPSILON);

    // 13.0 + 7.0 = 20.0 → trusted（下边界）
    storage
        .update_trait_confidence(t2, 0.9, 7.0, 0.5)
        .await
        .expect("更新证据量应成功");
    let status = engine
        .profile_status("char-0001")
        .await
        .expect("状态读取应成功");
    assert_eq!(status.status, "trusted");
    assert!((status.n_total_eff - 20.0).abs() < f64::EPSILON);
    assert!(
        status.status_text.contains("可信画像"),
        "文案: {}",
        status.status_text
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 事实浏览：字段过滤 + 分页 + total 为分页前条数。
#[tokio::test]
async fn facts_filter_and_paginate() {
    let (engine, storage, dir) = engine_with_db("browse-facts").await;
    seed_persona(&storage, "char-0001").await;
    seed_fact(&storage, "char-0001", ProfileField::Interests, "喜欢露营").await;
    seed_fact(&storage, "char-0001", ProfileField::Interests, "喜欢摄影").await;
    seed_fact(&storage, "char-0001", ProfileField::BasicInfo, "住在杭州").await;

    // 全字段：3 条
    let page = engine
        .memory_facts(FactBrowseRequest {
            persona: "char-0001".to_string(),
            field: None,
            limit: None,
            offset: None,
        })
        .await
        .expect("事实浏览应成功");
    assert_eq!(page.total, 3);
    assert_eq!(page.items.len(), 3);

    // 字段过滤：仅兴趣爱好 2 条
    let page = engine
        .memory_facts(FactBrowseRequest {
            persona: "char-0001".to_string(),
            field: Some(ProfileField::Interests),
            limit: None,
            offset: None,
        })
        .await
        .expect("事实浏览应成功");
    assert_eq!(page.total, 2);
    assert!(
        page.items
            .iter()
            .all(|f| f.field == ProfileField::Interests)
    );

    // 分页：limit 1 offset 1 → 1 条，total 保留
    let page = engine
        .memory_facts(FactBrowseRequest {
            persona: "char-0001".to_string(),
            field: None,
            limit: Some(1),
            offset: Some(1),
        })
        .await
        .expect("事实浏览应成功");
    assert_eq!(page.total, 3);
    assert_eq!(page.items.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 事实详情与分组：版本链仅在多版本时入版本表。
#[tokio::test]
async fn fact_detail_and_grouped_version_chains() {
    let (engine, storage, dir) = engine_with_db("browse-fact-detail").await;
    seed_persona(&storage, "char-0001").await;

    // 版本链：old → new（覆盖写）
    let mut old = PersonaFact::new(
        "char-0001".to_string(),
        ProfileField::Interests,
        "喜欢摄影".to_string(),
        FactSource::Manual,
    );
    let old_id = storage.save_fact(&old).await.expect("写入旧事实应成功");
    old.id = old_id;
    let fresh = PersonaFact::new(
        "char-0001".to_string(),
        ProfileField::Interests,
        "喜欢旅行".to_string(),
        FactSource::Manual,
    );
    let fresh_id = storage
        .save_fact_with_version(&old, &fresh)
        .await
        .expect("覆盖写应成功");
    // 单版本事实（不入版本表）
    let single = seed_fact(&storage, "char-0001", ProfileField::BasicInfo, "住在杭州").await;

    // 详情：版本链含旧、新两条（链头最早在前）
    let detail = engine
        .memory_fact_detail(fresh_id)
        .await
        .expect("详情读取应成功")
        .expect("事实应存在");
    assert_eq!(detail.fact.id, fresh_id);
    assert_eq!(detail.versions.len(), 2);
    assert_eq!(detail.versions[0].id, old_id, "链头最早在前");
    assert_eq!(detail.versions[1].id, fresh_id);

    // 不存在：None
    assert!(
        engine
            .memory_fact_detail(99_999)
            .await
            .expect("详情读取应成功")
            .is_none()
    );

    // 分组：Interests 组含新事实；版本表只含多版本事实
    let grouped = engine
        .memory_facts_grouped("char-0001")
        .await
        .expect("分组读取应成功");
    let interest = grouped
        .grouped
        .get(ProfileField::Interests.label())
        .expect("兴趣爱好分组应存在");
    assert_eq!(interest.len(), 1);
    assert_eq!(interest[0].id, fresh_id);
    assert_eq!(grouped.versions.len(), 1);
    assert!(grouped.versions.contains_key(&fresh_id));
    assert!(
        !grouped.versions.contains_key(&single),
        "单版本事实不入版本表"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 证据链：三类统计 + 事件来源链 + 空链与参数校验。
#[tokio::test]
async fn trait_evidence_chain_and_boundaries() {
    let (engine, storage, dir) = engine_with_db("browse-evidence").await;
    seed_persona(&storage, "char-0001").await;
    let trait_id = seed_trait(&storage, "char-0001", "温和", TraitLayer::Base, 1, 2.0).await;

    // 事件一（带 L1 溯源与证据片段）；事件二（无溯源）
    let event1 = seed_event(&storage, "char-0001", "事件一", 1_000).await;
    let event2 = seed_event(&storage, "char-0001", "事件二", 2_000).await;

    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(
        session.id,
        "备考期间保持耐心".to_string(),
        Some("夜间".to_string()),
    );
    l1.persona_uid = Some("char-0001".to_string());
    l1.evidence_notes = Some(vec![EvidenceNote::new("连续几天复习到深夜")]);
    l1.created_at = 1_500;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    storage
        .save_event_source(event1, l1.id, 0.8)
        .await
        .expect("写入事件溯源应成功");

    // 证据：support ×1 + contradict ×1 + neutral ×1
    for (event_id, direction) in [
        (event1, EvidenceDirection::Support),
        (event2, EvidenceDirection::Contradict),
        (event2, EvidenceDirection::Neutral),
    ] {
        let evidence = TraitEvidence::new(trait_id, event_id, direction, 0.8);
        storage
            .save_evidence(&evidence)
            .await
            .expect("写入证据应成功");
    }

    let chains = engine
        .memory_trait_evidence(TraitEvidenceRequest {
            persona: "char-0001".to_string(),
            trait_id,
        })
        .await
        .expect("证据链应成功");
    assert_eq!(chains.len(), 1);
    let chain = &chains[0];
    assert_eq!(chain.trait_id, trait_id);
    assert_eq!(chain.trait_label, "温和");
    assert_eq!(chain.total_evidence, 3);
    assert_eq!(chain.support_count, 1);
    assert_eq!(chain.contradict_count, 1);
    assert_eq!(chain.neutral_count, 1);
    assert_eq!(chain.evidence_events.len(), 3);
    let ev1 = chain
        .evidence_events
        .iter()
        .find(|e| e.event_id == event1)
        .expect("事件一应出现");
    assert_eq!(ev1.l1_sources.len(), 1);
    assert_eq!(
        ev1.l1_sources[0].evidence_notes,
        vec!["连续几天复习到深夜".to_string()]
    );
    assert!((ev1.l1_sources[0].weight - 0.8).abs() < f64::EPSILON);
    assert_eq!(ev1.l1_sources[0].l1_id, l1.id);

    // 空证据链：无证据标签返回单条空链
    let bare = seed_trait(&storage, "char-0001", "未验证", TraitLayer::Accent, 2, 0.0).await;
    let chains = engine
        .memory_trait_evidence(TraitEvidenceRequest {
            persona: "char-0001".to_string(),
            trait_id: bare,
        })
        .await
        .expect("证据链应成功");
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].total_evidence, 0);
    assert!(chains[0].evidence_events.is_empty());

    // trait_id <= 0 / 空 persona：业务校验错误
    let err = engine
        .memory_trait_evidence(TraitEvidenceRequest {
            persona: "char-0001".to_string(),
            trait_id: 0,
        })
        .await
        .expect_err("非法 trait_id 应报错");
    assert_eq!(err.category(), "validation");
    let err = engine
        .memory_trait_evidence(TraitEvidenceRequest {
            persona: "  ".to_string(),
            trait_id,
        })
        .await
        .expect_err("空 persona 应报错");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话列表：消息计数聚合 + 开始时间倒序 + 分页 + total + 分页钳制。
#[tokio::test]
async fn sessions_aggregate_counts_and_paginate() {
    let (engine, storage, dir) = engine_with_db("browse-sessions").await;
    seed_persona(&storage, "char-0001").await;

    let s1 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    tokio::time::sleep(Duration::from_millis(2)).await;
    let s2 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    tokio::time::sleep(Duration::from_millis(2)).await;
    let s3 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    seed_messages(&storage, s1.id, "char-0001", 2, 1_000).await;
    seed_messages(&storage, s3.id, "char-0001", 3, 3_000).await;
    // s2 无消息：计数应为 0

    // 全量：按开始时间倒序（s3 → s2 → s1），计数聚合正确
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    assert_eq!(page.total, 3);
    let ids: Vec<Uuid> = page.items.iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![s3.id, s2.id, s1.id]);
    let counts: Vec<u32> = page.items.iter().map(|s| s.message_count).collect();
    assert_eq!(counts, vec![3, 0, 2], "无消息会话计数按 0");
    assert!(page.items[1].ended_at.is_none());
    assert_eq!(page.items[0].channel, "local");

    // 分页：limit 2 → 2 条；offset 2 → 1 条
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: Some(2),
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    assert_eq!(page.total, 3);
    assert_eq!(page.items.len(), 2);
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: Some(2),
            offset: Some(2),
        })
        .await
        .expect("会话列表应成功");
    assert_eq!(page.items.len(), 1);

    // 分页钳制：limit 0 → 下界 1
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: Some(0),
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    assert_eq!(page.items.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话未读口径：本地助手消息计入；用户发言与导入历史不计；
/// 标记已读后归零，标记后的新助手消息重新计入。
#[tokio::test]
async fn session_unread_counts_local_assistant_and_mark_read() {
    let (engine, storage, dir) = engine_with_db("browse-unread").await;
    seed_persona(&storage, "char-0001").await;

    // 空库：未读总数 0
    assert_eq!(
        engine.unread_total().await.expect("未读总数应成功"),
        0,
        "空库未读应为 0"
    );

    let s1 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let s2 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // s1：用户发言（不计）+ 本地助手（计）+ 带指纹的助手（导入历史，不计）+ 本地助手（计）
    let mut m_user = Message::new(
        s1.id,
        MessageRole::User,
        "用户发言".to_string(),
        MessageSource::Local,
    );
    m_user.created_at = 1_000;
    storage
        .save_message(&m_user)
        .await
        .expect("写入用户消息应成功");
    for (content, ts, fingerprint) in [
        ("回复一", 1_001_i64, None),
        ("导入回复", 1_002, Some("fp-import-1")),
        ("回复二", 1_003, None),
    ] {
        let mut m = Message::new(
            s1.id,
            MessageRole::Assistant,
            content.to_string(),
            MessageSource::Online,
        )
        .with_persona_uid(Some("char-0001".to_string()));
        m.created_at = ts;
        m.fingerprint = fingerprint.map(str::to_string);
        storage.save_message(&m).await.expect("写入助手消息应成功");
    }
    // s2 无消息：未读为 0

    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    let s1_item = page
        .items
        .iter()
        .find(|s| s.id == s1.id)
        .expect("s1 应出现");
    assert_eq!(
        s1_item.unread, 2,
        "本地助手消息应计入未读（用户发言与导入历史不计）"
    );
    let s2_item = page
        .items
        .iter()
        .find(|s| s.id == s2.id)
        .expect("s2 应出现");
    assert_eq!(s2_item.unread, 0, "无消息会话未读为 0");
    assert_eq!(engine.unread_total().await.expect("未读总数应成功"), 2);

    // 分页路径同样填充未读（两条同批创建，按 id 查找不依赖排序位置）
    let paged = engine
        .session_list(SessionBrowseRequest {
            limit: Some(2),
            offset: None,
        })
        .await
        .expect("分页应成功");
    assert_eq!(paged.items.len(), 2);
    assert_eq!(
        paged.items.iter().find(|s| s.id == s1.id).map(|s| s.unread),
        Some(2),
        "分页路径未读应正确"
    );

    // 标记已读：未读归零
    engine
        .session_mark_read(s1.id)
        .await
        .expect("标记已读应成功");
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    assert_eq!(
        page.items.iter().find(|s| s.id == s1.id).map(|s| s.unread),
        Some(0),
        "标记后未读应归零"
    );
    assert_eq!(engine.unread_total().await.expect("未读总数应成功"), 0);

    // 幂等：不存在会话标记成功
    engine
        .session_mark_read(Uuid::new_v4())
        .await
        .expect("不存在的会话应幂等成功");

    // 标记后新到的助手消息重新计未读（时间晚于已读时间戳）
    let mut newer = Message::new(
        s1.id,
        MessageRole::Assistant,
        "新回复".to_string(),
        MessageSource::Online,
    );
    newer.created_at = ramaria_core::types::now_ms() + 10_000;
    storage
        .save_message(&newer)
        .await
        .expect("写入新消息应成功");
    assert_eq!(
        engine.unread_total().await.expect("未读总数应成功"),
        1,
        "标记后的新助手消息应重新计入未读"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 未读汇总：跨会话求和；标记单个会话只归零该会话。
#[tokio::test]
async fn unread_total_sums_across_sessions() {
    let (engine, storage, dir) = engine_with_db("browse-unread-total").await;
    seed_persona(&storage, "char-0001").await;

    let s1 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let s2 = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // s1 两条 + s2 一条本地助手消息
    for (session_id, ts) in [(s1.id, 1_001_i64), (s1.id, 1_002), (s2.id, 1_003)] {
        let mut m = Message::new(
            session_id,
            MessageRole::Assistant,
            "回复".to_string(),
            MessageSource::Online,
        );
        m.created_at = ts;
        storage.save_message(&m).await.expect("写入助手消息应成功");
    }
    assert_eq!(
        engine.unread_total().await.expect("汇总应成功"),
        3,
        "应跨会话求和"
    );

    // 只标记 s1：s2 的未读保留
    engine
        .session_mark_read(s1.id)
        .await
        .expect("标记已读应成功");
    assert_eq!(
        engine.unread_total().await.expect("汇总应成功"),
        1,
        "标记单个会话只归零该会话"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话消息：全量 / 分页翻正 / 钳制 / 负偏移 / has_more 边界 / 不存在会话报错。
#[tokio::test]
async fn session_messages_full_and_paged() {
    let (engine, storage, dir) = engine_with_db("browse-messages").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    seed_messages(&storage, session.id, "char-0001", 5, 1_000).await;

    // 全量：时间正序、total 5、has_more false
    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: None,
            offset: None,
        })
        .await
        .expect("消息浏览应成功");
    assert_eq!(view.total, 5);
    assert!(!view.has_more);
    let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec![
            "消息内容 0",
            "消息内容 1",
            "消息内容 2",
            "消息内容 3",
            "消息内容 4"
        ]
    );
    assert_eq!(view.messages[0].role, MessageRole::User);
    assert_eq!(view.messages[0].source, MessageSource::Local);
    assert_eq!(view.messages[0].persona_uid.as_deref(), Some("char-0001"));

    // 分页：limit 2 → 最新 2 条（页内时间正序）、has_more true
    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: Some(2),
            offset: None,
        })
        .await
        .expect("消息浏览应成功");
    let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["消息内容 3", "消息内容 4"]);
    assert_eq!(view.total, 5);
    assert!(view.has_more);

    // 不整除边界：offset 2 + limit 2 → has_more true
    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: Some(2),
            offset: Some(2),
        })
        .await
        .expect("消息浏览应成功");
    let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["消息内容 1", "消息内容 2"]);
    assert!(view.has_more);

    // 正好整除边界：offset 3 + limit 2 → has_more false
    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: Some(2),
            offset: Some(3),
        })
        .await
        .expect("消息浏览应成功");
    let contents: Vec<&str> = view.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["消息内容 0", "消息内容 1"]);
    assert!(!view.has_more);

    // 钳制与负偏移：limit 0 → 1 条；offset 负数按 0 处理
    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: Some(0),
            offset: Some(-3),
        })
        .await
        .expect("消息浏览应成功");
    assert_eq!(view.messages.len(), 1);
    assert_eq!(view.messages[0].content, "消息内容 4");
    assert!(view.has_more);

    // 会话不存在：业务校验错误（入口无需预判）
    let err = engine
        .session_messages(SessionMessagesRequest {
            session_id: Uuid::new_v4(),
            limit: None,
            offset: None,
        })
        .await
        .expect_err("不存在的会话应显式报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("会话不存在"),
        "错误文案应含会话不存在: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话消息：会话存在但无消息 → 空集合 + total 0 + has_more false（非错误）。
#[tokio::test]
async fn session_messages_for_existing_empty_session() {
    let (engine, storage, dir) = engine_with_db("browse-messages-empty").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    let view = engine
        .session_messages(SessionMessagesRequest {
            session_id: session.id,
            limit: None,
            offset: None,
        })
        .await
        .expect("空会话应返回空消息集合");
    assert_eq!(view.session_id, session.id);
    assert_eq!(view.total, 0);
    assert!(view.messages.is_empty());
    assert!(!view.has_more);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话详情：元数据 + 消息页字段逐项；分页 has_more 口径与消息浏览一致。
#[tokio::test]
async fn session_detail_returns_metadata_and_message_page() {
    let (engine, storage, dir) = engine_with_db("browse-session-detail").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    seed_messages(&storage, session.id, "char-0001", 5, 1_000).await;

    // 全量：元数据透传 + 消息时间正序 + has_more false
    let detail = engine
        .session_detail(session.id, None, None)
        .await
        .expect("详情读取应成功");
    assert_eq!(detail.id, session.id);
    assert_eq!(detail.started_at.timestamp_millis(), session.started_at);
    assert_eq!(detail.ended_at, None);
    assert_eq!(detail.persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(detail.total_messages, 5);
    assert!(!detail.has_more);
    let contents: Vec<&str> = detail.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents[0], "消息内容 0");
    assert_eq!(contents[4], "消息内容 4");

    // 分页：最新 2 条（页内时间正序）+ has_more true
    let paged = engine
        .session_detail(session.id, Some(2), None)
        .await
        .expect("详情读取应成功");
    assert_eq!(paged.total_messages, 5);
    assert!(paged.has_more);
    let contents: Vec<&str> = paged.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["消息内容 3", "消息内容 4"]);

    // 不存在会话：业务校验错误
    let err = engine
        .session_detail(Uuid::new_v4(), None, None)
        .await
        .expect_err("不存在的会话应显式报错");
    assert_eq!(err.category(), "validation");
    assert!(err.to_string().contains("会话不存在"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话消息计数：正常计数；不存在会话按 0（诊断口径，不报错）。
#[tokio::test]
async fn count_session_messages_tolerates_missing_session() {
    let (engine, storage, dir) = engine_with_db("browse-count-messages").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    seed_messages(&storage, session.id, "char-0001", 3, 1_000).await;

    assert_eq!(engine.count_session_messages(session.id).await, 3);
    assert_eq!(engine.count_session_messages(Uuid::new_v4()).await, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 按会话读取 L1：空会话返回空列表；写入后字段与存储一致（不报错）。
#[tokio::test]
async fn l1_by_session_returns_session_summaries() {
    let (engine, storage, dir) = engine_with_db("browse-l1-by-session").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    // 空会话：空列表（非错误）
    let empty = engine
        .memory_l1_by_session(session_id)
        .await
        .expect("按会话查询应成功");
    assert!(empty.is_empty(), "无摘要会话应返回空列表");

    // 写入两条摘要：全部返回且字段透传
    for (idx, summary) in ["第一段", "第二段"].iter().enumerate() {
        let mut l1 = MemoryL1::new(session_id, summary.to_string(), None);
        l1.persona_uid = Some("char-0001".to_string());
        l1.created_at = 2_000 + idx as i64;
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    }

    let items = engine
        .memory_l1_by_session(session_id)
        .await
        .expect("按会话查询应成功");
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item.session_id == session_id));
    assert!(
        items
            .iter()
            .all(|item| item.persona_uid.as_deref() == Some("char-0001")),
        "归属应随摘要透传"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 通道概览：空通道为 0 / None；仅统计目标通道（local 不计入）；关闭后活跃数下降。
#[tokio::test]
async fn channel_overview_counts_active_and_latest_activity() {
    let (engine, storage, dir) = engine_with_db("browse-channel-overview").await;
    seed_persona(&storage, "char-0001").await;

    // 空通道：计数 0、无活动时间
    let empty = engine
        .channel_overview("mcp")
        .await
        .expect("统计空通道应成功");
    assert_eq!(empty.active_sessions, 0);
    assert_eq!(empty.last_activity_ms, None);

    // mcp 通道两个会话 + local 通道一个会话（local 不应计入 mcp 统计）
    seed_channel_session(&storage, "char-0001", "mcp", Some("client-A"), 1, 1_000).await;
    let s2 = seed_channel_session(&storage, "char-0001", "mcp", Some("client-B"), 1, 2_000).await;
    seed_channel_session(&storage, "char-0001", "local", None, 1, 9_000).await;

    let overview = engine
        .channel_overview("mcp")
        .await
        .expect("统计 mcp 通道应成功");
    assert_eq!(overview.active_sessions, 2, "两个 mcp 会话均活跃");
    assert_eq!(
        overview.last_activity_ms,
        Some(2_000),
        "最近活动取 mcp 通道消息，local 会话（9000）不计入"
    );

    // 关闭一个会话：活跃数下降；最近活动时间不受影响（消息仍在库中）
    storage.close_session(s2).await.expect("关闭会话应成功");
    let after = engine
        .channel_overview("mcp")
        .await
        .expect("再次统计应成功");
    assert_eq!(after.active_sessions, 1, "关闭一个会话后活跃数应下降");
    assert_eq!(after.last_activity_ms, Some(2_000));

    let _ = std::fs::remove_dir_all(&dir);
}
