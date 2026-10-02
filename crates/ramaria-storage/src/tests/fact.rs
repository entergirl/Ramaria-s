//! crates/ramaria-storage/src/tests/fact.rs - 人格事实（persona_facts）存储测试
//!
//! 设计特点:
//! - 覆盖事实 CRUD 与按字段 / 全量查询
//! - 覆盖事务化版本链覆盖写（旧版本置 superseded + version_of 指针）
//! - 覆盖重复覆盖的并发防护（返回冲突且不产生第二条 active）

use super::*;

#[tokio::test]
async fn persona_fact_crud() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let fact = PersonaFact::new(
        "user-0001".into(),
        ramaria_core::types::ProfileField::BasicInfo,
        "姓名：小明".into(),
        FactSource::L1,
    );
    let fact_id = storage.save_fact(&fact).await.unwrap();
    assert!(fact_id > 0);

    let facts = storage
        .list_facts_by_persona("user-0001", ramaria_core::types::ProfileField::BasicInfo)
        .await
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].id, fact_id);
}

// =========================================================
// persona_facts 版本化 repo 测试
// =========================================================

/// 事务化版本链覆盖写：旧事实置 superseded + 新事实写入（version_of 指向旧 id）。
#[tokio::test]
async fn fact_version_chain_overwrite_atomic() {
    use ramaria_core::types::FactStatus;
    let storage = setup().await;
    let p = Persona::new(
        "user-0002".into(),
        "用户二".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let mut old = PersonaFact::new(
        "user-0002".into(),
        ramaria_core::types::ProfileField::PersonalStatus,
        "当前情绪：平静".into(),
        FactSource::Event,
    );
    let old_id = storage.save_fact(&old).await.unwrap();
    old.id = old_id;

    // 新事实覆盖旧事实：旧置 superseded、新写入且 version_of 指向旧
    let fresh = PersonaFact::new(
        "user-0002".into(),
        ramaria_core::types::ProfileField::PersonalStatus,
        "当前情绪：焦虑".into(),
        FactSource::Event,
    );
    let fresh_id = storage.save_fact_with_version(&old, &fresh).await.unwrap();
    assert!(fresh_id > old_id);

    // 旧事实已 superseded
    let old_now = storage.get_fact_by_id(old_id).await.unwrap().unwrap();
    assert_eq!(old_now.status, FactStatus::Superseded);

    // 新事实 active 且 version_of 指向旧 id
    let fresh_now = storage.get_fact_by_id(fresh_id).await.unwrap().unwrap();
    assert_eq!(fresh_now.status, FactStatus::Active);
    assert_eq!(fresh_now.version_of, Some(old_id));

    // 版本链：从新事实回溯到旧事实（链头最早在前）
    let chain = storage.list_fact_versions(fresh_id).await.unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].id, old_id);
    assert_eq!(chain[1].id, fresh_id);

    // active 查询只返回新事实（不含 superseded 旧事实）
    let active = storage
        .list_active_facts_by_field(
            "user-0002",
            ramaria_core::types::ProfileField::PersonalStatus,
        )
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, fresh_id);
}

/// list_active_facts_by_persona：跨字段仅返回 active 事实（superseded 旧版本被排除）。
#[tokio::test]
async fn fact_list_active_by_persona_excludes_superseded() {
    use ramaria_core::types::FactStatus;
    let storage = setup().await;
    let p = Persona::new(
        "user-0003".into(),
        "用户三".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    // 先写入旧事实
    let mut old = PersonaFact::new(
        "user-0003".into(),
        ramaria_core::types::ProfileField::Interests,
        "喜欢摄影".into(),
        FactSource::Manual,
    );
    let old_id = storage.save_fact(&old).await.unwrap();
    old.id = old_id;

    // 覆盖写：旧事实自动置 superseded，新事实 active（版本链推进）
    let fresh = PersonaFact::new(
        "user-0003".into(),
        ramaria_core::types::ProfileField::Interests,
        "喜欢旅行".into(),
        FactSource::Manual,
    );
    let fresh_id = storage.save_fact_with_version(&old, &fresh).await.unwrap();
    assert!(fresh_id > old_id);

    let old_now = storage.get_fact_by_id(old_id).await.unwrap().unwrap();
    assert_eq!(old_now.status, FactStatus::Superseded);

    // active 查询只含新事实（不含 superseded 旧版本）
    let active = storage
        .list_active_facts_by_persona("user-0003")
        .await
        .unwrap();
    assert_eq!(active.len(), 1, "superseded 事实不应出现在 active 查询中");
    assert_eq!(active[0].id, fresh_id);

    // 全部查询（CLI/版本链统计）仍包含 superseded 旧版本
    let all = storage
        .list_all_facts_by_persona("user-0003")
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|f| f.status == FactStatus::Superseded));
    assert!(all.iter().any(|f| f.status == FactStatus::Active));
}

/// get_fact_by_id 对不存在 id 返回 None（CLI show 缺省兜底）。
#[tokio::test]
async fn fact_get_by_id_missing_returns_none() {
    let storage = setup().await;
    let got = storage.get_fact_by_id(99999).await.unwrap();
    assert!(got.is_none());
}

/// 验证 GROUP BY 查询正确统计各字段数量。
#[tokio::test]
async fn count_all_facts_for_persona_grouped() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    // 写入 3 条不同字段的 fact
    let f1 = PersonaFact::new(
        "user-0001".into(),
        ramaria_core::types::ProfileField::BasicInfo,
        "姓名：小明".into(),
        FactSource::L1,
    );
    let f2 = PersonaFact::new(
        "user-0001".into(),
        ramaria_core::types::ProfileField::Interests,
        "喜欢编程".into(),
        FactSource::Manual,
    );
    let f3 = PersonaFact::new(
        "user-0001".into(),
        ramaria_core::types::ProfileField::Interests,
        "喜欢阅读".into(),
        FactSource::Manual,
    );
    storage.save_fact(&f1).await.unwrap();
    storage.save_fact(&f2).await.unwrap();
    storage.save_fact(&f3).await.unwrap();

    let counts = storage
        .count_all_facts_for_persona("user-0001")
        .await
        .unwrap();
    assert_eq!(counts.len(), 7, "应返回全部 7 个 ProfileField");

    // BasicInfo: 1 条
    let basic_count = counts
        .iter()
        .find(|(f, _)| *f == ramaria_core::types::ProfileField::BasicInfo)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(basic_count, 1);

    // Interests: 2 条
    let interests_count = counts
        .iter()
        .find(|(f, _)| *f == ramaria_core::types::ProfileField::Interests)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(interests_count, 2);

    // PersonalStatus: 0 条（未写入）
    let ps_count = counts
        .iter()
        .find(|(f, _)| *f == ramaria_core::types::ProfileField::PersonalStatus)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(ps_count, 0, "未写入的字段应返回 0");

    // 总计数应为 3
    let total: usize = counts.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 3);
}

/// 验证无记录 persona 返回全 0。
#[tokio::test]
async fn count_all_facts_for_persona_empty() {
    let storage = setup().await;
    let p = Persona::new(
        "user-empty".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let counts = storage
        .count_all_facts_for_persona("user-empty")
        .await
        .unwrap();
    assert_eq!(counts.len(), 7);
    let total: usize = counts.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 0, "无 fact 时总计应为 0");
}

/// 版本链并发防护：同一旧事实被覆盖两次时，第二次返回冲突且不产生第二条 active。
#[tokio::test]
async fn fact_double_supersede_conflict() {
    let storage = setup().await;
    let p = Persona::new(
        "user-0004".into(),
        "用户四".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let mut old = PersonaFact::new(
        "user-0004".into(),
        ramaria_core::types::ProfileField::PersonalStatus,
        "当前情绪：平静".into(),
        FactSource::Event,
    );
    let old_id = storage.save_fact(&old).await.unwrap();
    old.id = old_id;

    // 第一次覆盖成功
    let fresh1 = PersonaFact::new(
        "user-0004".into(),
        ramaria_core::types::ProfileField::PersonalStatus,
        "当前情绪：焦虑".into(),
        FactSource::Event,
    );
    storage.save_fact_with_version(&old, &fresh1).await.unwrap();

    // 第二次仍用已 superseded 的 old：必须报冲突，且不得再写入 active
    let fresh2 = PersonaFact::new(
        "user-0004".into(),
        ramaria_core::types::ProfileField::PersonalStatus,
        "当前情绪：兴奋".into(),
        FactSource::Event,
    );
    let err = storage
        .save_fact_with_version(&old, &fresh2)
        .await
        .expect_err("已 superseded 的旧事实应拒绝再次覆盖");
    assert!(matches!(err, RamariaError::Validation { .. }));

    let active = storage
        .list_active_facts_by_field(
            "user-0004",
            ramaria_core::types::ProfileField::PersonalStatus,
        )
        .await
        .unwrap();
    assert_eq!(active.len(), 1, "同 field 只应保留一条 active");
    assert_eq!(active[0].content, "当前情绪：焦虑");
}
