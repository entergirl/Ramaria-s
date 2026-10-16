//! crates/ramaria-service/src/persona/allow_inference/tests.rs - Ramaria 人格推断接管用例单元测试
//!
//! 设计特点:
//! - 由 `persona::allow_inference` 以 `#[cfg(test)] mod tests;` 收纳：覆盖全量 / 按 label /
//!   幂等与空态边界 / 只改 Active 行裁量
//! - 使用真实 SQLite 临时库与 mock LLM：断言以落库值与改写计数为准
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问网络、OS keychain 与真实用户数据。

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus};
use ramaria_storage::SqliteStorage;

use crate::test_support::{engine_with_db, seed_persona};

// =========================================================
// 共享夹具
// =========================================================

/// 造一条指定来源 / 状态的 Base trait（返回存储层回填 id 后的完整行）。
async fn seed_trait(
    storage: &SqliteStorage,
    uid: &str,
    label: &str,
    meaning: &str,
    source: TraitSource,
    status: TraitStatus,
) -> PersonalityTrait {
    let mut t = PersonalityTrait::new(
        uid.into(),
        TraitLayer::Base,
        label.into(),
        meaning.into(),
        source,
        0,
    );
    t.status = status;
    let id = storage.save_trait(&t).await.expect("落库 trait 应成功");
    PersonalityTrait { id, ..t }
}

// =========================================================
// 全量与按 label 改写
// =========================================================

/// 全量接管：两条 Manual 改写为 Inferred（其余字段零变化），原 Inferred 行保持不动。
#[tokio::test]
async fn allow_inference_rewrites_all_manual_traits() {
    let (engine, storage, dir) = engine_with_db("allow-inference-all").await;
    seed_persona(&storage, "char-0001").await;

    let m1 = seed_trait(
        &storage,
        "char-0001",
        "温柔",
        "待人温和",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;
    let m2 = seed_trait(
        &storage,
        "char-0001",
        "慢热",
        "熟悉后放松",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;
    let i1 = seed_trait(
        &storage,
        "char-0001",
        "尽责",
        "对任务负责",
        TraitSource::Inferred,
        TraitStatus::Active,
    )
    .await;

    let updated = engine
        .persona_allow_inference("char-0001", &[])
        .await
        .expect("全量接管应成功");
    assert_eq!(updated, 2, "两条 Manual 应被改写");

    let traits = storage
        .list_traits_by_persona("char-0001")
        .await
        .expect("查询 traits 应成功");
    let by_id = |id: i64| traits.iter().find(|t| t.id == id).expect("行应存在于列表");

    for original in [&m1, &m2] {
        let row = by_id(original.id);
        assert_eq!(
            row.source,
            TraitSource::Inferred,
            "Manual 应改写为 Inferred"
        );
        assert_eq!(row.status, TraitStatus::Active, "状态保持");
        assert_eq!(row.trait_label, original.trait_label, "标签零变化");
        assert_eq!(row.meaning, original.meaning, "含义零变化");
        assert_eq!(row.confidence, original.confidence, "置信度零变化");
    }

    let inferred_row = by_id(i1.id);
    assert_eq!(
        inferred_row.source,
        TraitSource::Inferred,
        "原 Inferred 行应保持不动"
    );
    assert_eq!(inferred_row.meaning, i1.meaning);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 按 label 接管：只改写命中的标签，其余 Manual 行保持。
#[tokio::test]
async fn allow_inference_filters_by_label() {
    let (engine, storage, dir) = engine_with_db("allow-inference-label").await;
    seed_persona(&storage, "char-0001").await;

    let hit = seed_trait(
        &storage,
        "char-0001",
        "温柔",
        "待人温和",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;
    let miss = seed_trait(
        &storage,
        "char-0001",
        "慢热",
        "熟悉后放松",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;

    let labels = vec!["温柔".to_string()];
    let updated = engine
        .persona_allow_inference("char-0001", &labels)
        .await
        .expect("按 label 接管应成功");
    assert_eq!(updated, 1, "只应改写命中标签");

    let traits = storage
        .list_traits_by_persona("char-0001")
        .await
        .expect("查询 traits 应成功");
    let hit_row = traits
        .iter()
        .find(|t| t.id == hit.id)
        .expect("命中行应存在");
    let miss_row = traits
        .iter()
        .find(|t| t.id == miss.id)
        .expect("未命中行应存在");
    assert_eq!(hit_row.source, TraitSource::Inferred);
    assert_eq!(
        miss_row.source,
        TraitSource::Manual,
        "未命中 label 应保持 Manual"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 边界与裁量
// =========================================================

/// 边界：未知 uid / 未知 label / 重复调用均零改写且非错误；重复调用幂等。
#[tokio::test]
async fn allow_inference_edges_return_zero() {
    let (engine, storage, dir) = engine_with_db("allow-inference-edges").await;
    seed_persona(&storage, "char-0001").await;
    seed_trait(
        &storage,
        "char-0001",
        "温柔",
        "待人温和",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;

    let unknown_uid = engine
        .persona_allow_inference("char-9999", &[])
        .await
        .expect("未知 uid 不应报错");
    assert_eq!(unknown_uid, 0);

    let unknown_labels = vec!["不存在".to_string()];
    let unknown_label = engine
        .persona_allow_inference("char-0001", &unknown_labels)
        .await
        .expect("未知 label 不应报错");
    assert_eq!(unknown_label, 0);
    let traits = storage
        .list_traits_by_persona("char-0001")
        .await
        .expect("查询 traits 应成功");
    assert!(
        traits.iter().all(|t| t.source == TraitSource::Manual),
        "未命中标签不应改写"
    );

    let first = engine
        .persona_allow_inference("char-0001", &[])
        .await
        .expect("首次全量接管应成功");
    assert_eq!(first, 1);
    let second = engine
        .persona_allow_inference("char-0001", &[])
        .await
        .expect("重复调用不应报错");
    assert_eq!(second, 0, "重复调用应幂等为 0");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 裁量锁定：只改 Active 行；Deprecated 的 Manual 行不参与改写。
#[tokio::test]
async fn allow_inference_skips_deprecated_manual_rows() {
    let (engine, storage, dir) = engine_with_db("allow-inference-deprecated").await;
    seed_persona(&storage, "char-0001").await;

    let active = seed_trait(
        &storage,
        "char-0001",
        "温柔",
        "待人温和",
        TraitSource::Manual,
        TraitStatus::Active,
    )
    .await;
    let deprecated = seed_trait(
        &storage,
        "char-0001",
        "过时标签",
        "旧描述",
        TraitSource::Manual,
        TraitStatus::Deprecated,
    )
    .await;

    let updated = engine
        .persona_allow_inference("char-0001", &[])
        .await
        .expect("全量接管应成功");
    assert_eq!(updated, 1, "只应改写 Active 的 Manual 行");

    let traits = storage
        .list_traits_by_persona("char-0001")
        .await
        .expect("查询 traits 应成功");
    let active_row = traits
        .iter()
        .find(|t| t.id == active.id)
        .expect("Active 行应存在");
    assert_eq!(active_row.source, TraitSource::Inferred);

    // Deprecated 行不进入 list（只返回 active），直连库读回来源
    let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
        .await
        .expect("打开测试库应成功");
    let source: String = sqlx::query_scalar("SELECT source FROM personality_traits WHERE id = ?")
        .bind(deprecated.id)
        .fetch_one(&pool)
        .await
        .expect("直连查询应成功");
    assert_eq!(source, "manual", "Deprecated 行来源应保持 Manual");
    pool.close().await;

    let _ = std::fs::remove_dir_all(&dir);
}
