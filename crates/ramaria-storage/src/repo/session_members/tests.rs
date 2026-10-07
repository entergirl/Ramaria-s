//! crates/ramaria-storage/src/repo/session_members/tests.rs - 会话成员行存取单元测试
//!
//! 设计特点:
//! - 覆盖 upsert 合并口径：名称 / 群名片 / 角色保留与首末见时间单调
//! - 覆盖排序（首见升序）与跨会话隔离
//! - 覆盖非法角色字符串的读取降级与空批次直接成功

use super::*;
use crate::database::init_test_pool;

/// 插入 session fixture，满足 session_members 的外键约束。
async fn setup_session(pool: &SqlitePool) -> Uuid {
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
    session_id
}

fn member(
    session_id: Uuid,
    platform_ref: &str,
    name: &str,
    first: i64,
    last: i64,
) -> SessionMember {
    SessionMember::new(session_id, platform_ref, name, first, last)
}

/// 同键重复 upsert 合并为一行：名称取新值、首见取小、末见取大。
#[tokio::test]
async fn upsert_inserts_and_merges_on_conflict() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;

    upsert_batch(&pool, &[member(session_id, "u_a", "旧名", 100, 200)])
        .await
        .expect("首次 upsert 应成功");
    upsert_batch(&pool, &[member(session_id, "u_a", "新名", 50, 300)])
        .await
        .expect("冲突 upsert 应成功");

    let rows = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    assert_eq!(rows.len(), 1, "同键重复写入不应新增行");
    assert_eq!(rows[0].name, "新名");
    assert_eq!(rows[0].first_seen_at, 50, "首见时间应取小");
    assert_eq!(rows[0].last_seen_at, 300, "末见时间应取大");
}

/// 空名 upsert 不清空已记录名称，时间扩展仍生效。
#[tokio::test]
async fn upsert_keeps_existing_name_when_new_empty() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;

    upsert_batch(&pool, &[member(session_id, "u_a", "有名字", 100, 200)])
        .await
        .expect("首次 upsert 应成功");
    upsert_batch(&pool, &[member(session_id, "u_a", "", 50, 300)])
        .await
        .expect("空名 upsert 应成功");

    let rows = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    assert_eq!(rows[0].name, "有名字", "空名不应清空已记录名称");
    assert_eq!(rows[0].first_seen_at, 50, "时间扩展应生效");
    assert_eq!(rows[0].last_seen_at, 300);
}

/// 群名片 / 角色补全后不被缺失值清空；非法角色直写脏数据读取降级为 None。
#[tokio::test]
async fn upsert_fills_group_nickname_and_role_without_clearing() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;

    // 首次：群名片与角色均缺失
    upsert_batch(&pool, &[member(session_id, "u_a", "名字", 100, 200)])
        .await
        .expect("首次 upsert 应成功");

    // 补全：群名片与角色取新值
    let mut with_role = member(session_id, "u_a", "", 100, 200);
    with_role.group_nickname = Some("群名片".to_string());
    with_role.role = Some(MemberRole::Owner);
    upsert_batch(&pool, &[with_role])
        .await
        .expect("补全 upsert 应成功");
    let rows = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    assert_eq!(rows[0].group_nickname.as_deref(), Some("群名片"));
    assert_eq!(rows[0].role, Some(MemberRole::Owner));

    // 再缺失：不应清空已记录的群名片与角色
    upsert_batch(&pool, &[member(session_id, "u_a", "", 100, 200)])
        .await
        .expect("缺失值 upsert 应成功");
    let rows = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    assert_eq!(
        rows[0].group_nickname.as_deref(),
        Some("群名片"),
        "缺失群名片不应清空已记录值"
    );
    assert_eq!(rows[0].role, Some(MemberRole::Owner), "缺失角色不应清空");

    // 非法角色直写脏数据 → 读取降级为 None（记录 WARNING，不阻塞读取）
    sqlx::query("UPDATE session_members SET role = 'superuser' WHERE session_id = ?")
        .bind(session_id.to_string())
        .execute(&pool)
        .await
        .expect("直写脏数据应成功");
    let rows = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    assert_eq!(rows[0].role, None, "非法角色应降级为 None");
}

/// 按首见时间升序返回，且跨会话严格隔离。
#[tokio::test]
async fn list_by_session_orders_and_isolates() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_a = setup_session(&pool).await;
    let session_b = setup_session(&pool).await;

    // 乱序插入：首见时间与插入顺序不一致
    upsert_batch(
        &pool,
        &[
            member(session_a, "u_late", "晚", 300, 400),
            member(session_a, "u_early", "早", 100, 500),
        ],
    )
    .await
    .expect("会话 a 成员写入应成功");
    upsert_batch(&pool, &[member(session_b, "u_other", "另会话", 50, 60)])
        .await
        .expect("会话 b 成员写入应成功");

    let rows = list_by_session(&pool, session_a).await.expect("读取应成功");
    let refs: Vec<&str> = rows.iter().map(|r| r.platform_ref.as_str()).collect();
    assert_eq!(refs, vec!["u_early", "u_late"], "应按首见时间升序");

    let rows_b = list_by_session(&pool, session_b).await.expect("读取应成功");
    assert_eq!(rows_b.len(), 1, "跨会话成员不应混入");
    assert_eq!(rows_b[0].platform_ref, "u_other");
}

/// 空批次直接成功（不开启事务、不访问数据库）。
#[tokio::test]
async fn upsert_empty_batch_is_ok() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    upsert_batch(&pool, &[]).await.expect("空批次应直接成功");
}
