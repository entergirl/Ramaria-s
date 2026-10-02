//! crates/ramaria-storage/src/tests/background_job.rs - 后台任务存储测试
//!
//! 设计特点:
//! - 覆盖任务创建与待处理列表读取
//! - 覆盖状态更新后离开待处理列表
//! - 覆盖原子抢占的首次成功 / 重复失败 / 不存在返回 false
//! - 覆盖失败任务记录错误信息与空 payload 任务

use super::*;

// =========================================================
// background_jobs CRUD 集成测试
// =========================================================

#[tokio::test]
async fn background_job_create_and_list_pending() {
    let storage = setup().await;

    // 创建两个不同类型、不同 payload 的 job
    let id1 = storage
        .create_background_job("l2_extraction", Some(r#"{"session_id":"abc"}"#))
        .await
        .unwrap();
    let id2 = storage
        .create_background_job("personality_inference", Some(r#"{"persona_uid":"u1"}"#))
        .await
        .unwrap();

    assert!(id1 > 0, "job ID 应为正整数");
    assert!(id2 > 0, "job ID 应为正整数");
    assert_ne!(id1, id2, "不同 job 应有不同 ID");

    // list_pending 应包含两个 job
    let pending = storage.list_pending_jobs().await.unwrap();
    assert_eq!(pending.len(), 2);

    // 验证 job_type 和 payload 正确返回
    let job1 = pending.iter().find(|(id, _, _)| *id == id1).unwrap();
    assert_eq!(job1.1, "l2_extraction");
    assert_eq!(job1.2.as_deref(), Some(r#"{"session_id":"abc"}"#));

    let job2 = pending.iter().find(|(id, _, _)| *id == id2).unwrap();
    assert_eq!(job2.1, "personality_inference");
}

#[tokio::test]
async fn background_job_update_status_removes_from_pending() {
    let storage = setup().await;

    let id = storage
        .create_background_job("reindex", None)
        .await
        .unwrap();

    // 更新状态为 running，job 应从 pending 列表中移除
    storage
        .update_job_status(id, "running", None)
        .await
        .unwrap();

    let pending = storage.list_pending_jobs().await.unwrap();
    assert!(
        !pending.iter().any(|(jid, _, _)| *jid == id),
        "running 状态的 job 不应出现在 pending 列表中"
    );
}

/// 原子抢占：首次成功、重复失败、不存在返回 false（多消费方并发补扫去重的基础）。
#[tokio::test]
async fn background_job_claim_pending_is_atomic() {
    let storage = setup().await;

    let id = storage
        .create_background_job("l1_summary_retry", Some(r#"{"session_id":"abc"}"#))
        .await
        .unwrap();

    // 首次抢占成功 → 任务离开 pending 列表
    assert!(
        storage.claim_pending_job(id).await.unwrap(),
        "首次抢占应成功"
    );
    let pending = storage.list_pending_jobs().await.unwrap();
    assert!(
        !pending.iter().any(|(jid, _, _)| *jid == id),
        "抢占成功后任务不应出现在 pending 列表"
    );

    // 二次抢占失败（状态已不是 pending）
    assert!(
        !storage.claim_pending_job(id).await.unwrap(),
        "重复抢占应返回 false"
    );

    // 不存在的任务 → false（不报错，调用方可安全跳过）
    assert!(
        !storage.claim_pending_job(999_999).await.unwrap(),
        "不存在的任务应返回 false"
    );
}

#[tokio::test]
async fn background_job_with_error() {
    let storage = setup().await;

    let id = storage
        .create_background_job("data_migration", Some(r#"{"version":2}"#))
        .await
        .unwrap();

    // 更新为 failed 并记录错误信息
    storage
        .update_job_status(id, "failed", Some("磁盘空间不足"))
        .await
        .unwrap();

    // failed 的 job 也不应在 pending 中
    let pending = storage.list_pending_jobs().await.unwrap();
    assert!(
        !pending.iter().any(|(jid, _, _)| *jid == id),
        "failed 状态的 job 不应出现在 pending 列表中"
    );
}

#[tokio::test]
async fn background_job_empty_payload() {
    let storage = setup().await;

    let id = storage
        .create_background_job("health_check", None)
        .await
        .unwrap();

    let pending = storage.list_pending_jobs().await.unwrap();
    let job = pending.iter().find(|(jid, _, _)| *jid == id).unwrap();
    assert_eq!(job.1, "health_check");
    assert!(job.2.is_none(), "无 payload 时应为 None");
}
