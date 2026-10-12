//! crates/ramaria-service/src/bridge/tests.rs - 会话桥接单元测试
//!
//! 设计特点:
//! - 覆盖开关 / 原文白名单（显式载体）/ 两级降级 / 预算截断四组路径，断言以桥接结果为实
//! - 使用真实 SQLite 临时库与批内造数 helper，不依赖网络与模型文件
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use super::*;
use crate::test_support::{engine_with_db, seed_persona, seed_persona_kind, seed_utt_block};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MessageRole, MessageSource, PersonaKind};

fn bridge_cfg(enabled: bool) -> BridgeConfig {
    BridgeConfig {
        enabled,
        max_chars: 800,
    }
}

fn utt_cfg() -> UttConfig {
    // 白名单 = 角色类（char），与默认一致
    UttConfig::default()
}

/// 造一条消息（角色 / 内容 / 发言人归属 / 时间由调用方指定）。
async fn seed_message(
    storage: &ramaria_storage::SqliteStorage,
    session_id: uuid::Uuid,
    role: MessageRole,
    content: &str,
    persona: Option<&str>,
    created_at: i64,
) {
    let mut message = Message::new(session_id, role, content.to_string(), MessageSource::Local)
        .with_persona_uid(persona.map(str::to_string));
    message.created_at = created_at;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
}

/// 桥接开关关闭 → 不加载（验收：开关）。
#[tokio::test]
async fn bridge_disabled_returns_none() {
    let (_engine, storage, dir) = engine_with_db("bridge-disabled").await;
    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(false),
        &utt_cfg(),
        Some("char-0001"),
    )
    .await;
    assert!(ctx.is_empty(), "enabled=false 不应加载桥接");
    assert_eq!(ctx.source, None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单外 persona（助手类）→ 不加载（回归红线：与原文片段一致）。
#[tokio::test]
async fn bridge_skipped_for_non_whitelisted_persona() {
    let (_engine, storage, dir) = engine_with_db("bridge-whitelist").await;
    // rama 自身：Rama 类型不在角色白名单
    let ctx = load_bridge_context(storage.as_ref(), &bridge_cfg(true), &utt_cfg(), None).await;
    assert!(ctx.is_empty(), "助手类 persona 不应加载桥接原文");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 行 kind 在角色白名单内 → 加载（类型取 persona 行，不依赖 uid 前缀）。
#[tokio::test]
async fn bridge_loads_for_row_kind_without_standard_prefix() {
    let (_engine, storage, dir) = engine_with_db("bridge-row-kind").await;
    seed_persona_kind(&storage, "custom-1", PersonaKind::Char).await;
    let session = storage
        .create_session(Some("custom-1"))
        .await
        .expect("创建会话应成功");
    seed_message(
        &storage,
        session.id,
        MessageRole::User,
        "历史消息",
        None,
        1_700_000_000_000,
    )
    .await;
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");

    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("custom-1"),
    )
    .await;
    assert_eq!(
        ctx.source,
        Some(BridgeSource::RecentMessages),
        "行 kind 在角色白名单内应加载桥接"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// persona 行缺失 → 跳过（类型未知保守不放行，即使会话侧数据齐备）。
#[tokio::test]
async fn bridge_skipped_for_missing_persona_row() {
    let (_engine, storage, dir) = engine_with_db("bridge-missing-row").await;
    seed_persona(&storage, "char-0001").await;
    crate::test_support::seed_closed_session_with_messages(
        &storage,
        "char-0001",
        2,
        1_700_000_000_000,
    )
    .await;

    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("char-0009"),
    )
    .await;
    assert!(ctx.is_empty(), "persona 行缺失应跳过桥接");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无已关闭会话 → 跳过（验收：无会话跳过）。
#[tokio::test]
async fn bridge_no_closed_session_skips() {
    let (_engine, storage, dir) = engine_with_db("bridge-no-closed").await;
    seed_persona(&storage, "char-0001").await;
    storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建活跃会话应成功"); // 仅活跃会话

    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("char-0001"),
    )
    .await;
    assert!(ctx.is_empty(), "无已关闭会话不应加载桥接");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 一级来源：utt 块（验收：最近会话 + utt 块优先）。
#[tokio::test]
async fn bridge_loads_latest_utt_block_of_recent_closed_session() {
    let (_engine, storage, dir) = engine_with_db("bridge-utt").await;
    seed_persona(&storage, "char-0001").await;

    // 两个已关闭会话（ended_at 不同）：只取最近的一个（ended_at 最大）
    let older = crate::test_support::seed_closed_session_with_messages(
        &storage,
        "char-0001",
        1,
        1_700_000_000_000,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let newer = crate::test_support::seed_closed_session_with_messages(
        &storage,
        "char-0001",
        1,
        1_700_000_000_000,
    )
    .await;

    // 造数前提：新会话关闭时间必须晚于旧会话（否则取最近会话的断言无意义）
    let sessions = storage.list_sessions().await.expect("查询会话应成功");
    let ended_at_of = |id: uuid::Uuid| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.ended_at)
            .expect("会话应已关闭")
    };
    assert!(
        ended_at_of(newer) > ended_at_of(older),
        "造数前提：新会话关闭时间应晚于旧会话"
    );

    // 新会话有块、旧会话无块 → 加载新会话的块
    seed_utt_block(
        &storage,
        newer,
        "char-0001",
        "[2026-08-01 20:00] 测试人格: 上次聊到这里\n[2026-08-01 20:01] 用户: 好的",
    )
    .await;

    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("char-0001"),
    )
    .await;
    assert_eq!(ctx.source, Some(BridgeSource::UttBlock));
    let content = ctx.content.expect("应加载桥接内容");
    assert!(
        content.contains("上次聊到这里"),
        "应含 utt 块内容: {content}"
    );
    assert!(
        !content.contains("消息内容 0"),
        "不应回退到旧会话消息: {content}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 二级降级：无 utt 块 → 取末 N 条原文（验收：两级降级）。
#[tokio::test]
async fn bridge_falls_back_to_recent_messages() {
    let (_engine, storage, dir) = engine_with_db("bridge-fallback").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // 8 条消息：降级只取末 5 条（助手消息带人格、用户消息不带）
    for i in 0..8 {
        let (role, persona) = if i % 2 == 0 {
            (MessageRole::Assistant, Some("char-0001"))
        } else {
            (MessageRole::User, None)
        };
        seed_message(
            &storage,
            session.id,
            role,
            &format!("消息{i}"),
            persona,
            1_700_000_000_000 + i * 60_000,
        )
        .await;
    }
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");

    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("char-0001"),
    )
    .await;
    assert_eq!(ctx.source, Some(BridgeSource::RecentMessages));
    let content = ctx.content.expect("应加载桥接内容");
    assert!(
        content.contains("消息3"),
        "应含末 5 条的首条（消息3）: {content}"
    );
    assert!(content.contains("消息7"), "应含最后一条（消息7）");
    assert!(!content.contains("消息0"), "不应含被裁掉的早前消息");
    assert!(content.contains("测试人格"), "应含 persona 名称");
    assert!(content.contains("用户"), "应含用户角色标签");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 降级路径但会话无消息 → 跳过（验收：两级降级尽头）。
#[tokio::test]
async fn bridge_fallback_no_messages_skips() {
    let (_engine, storage, dir) = engine_with_db("bridge-empty").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");

    // 无 utt 块、无消息
    let ctx = load_bridge_context(
        storage.as_ref(),
        &bridge_cfg(true),
        &utt_cfg(),
        Some("char-0001"),
    )
    .await;
    assert!(ctx.is_empty(), "无块无消息应跳过桥接");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预算截断：超预算从头部截断、保最近（验收：预算截断）。
#[test]
fn truncate_from_head_keeps_recent_content() {
    let text = "第一行内容\n第二行内容\n第三行内容";
    let out = truncate_from_head(text, 10);
    assert!(out.contains("第三行内容"), "应保留最近内容: {out}");
    assert!(out.contains("前文已省略"), "应含省略标记");
    // 截断后实际内容（不含标记行）≤ 预算
    let body = out.split('\n').next_back().unwrap_or("");
    assert!(body.chars().count() <= 10, "保留内容不应超预算");
}

/// 预算充足 → 原样返回（不截断）。
#[test]
fn truncate_from_head_within_budget_unchanged() {
    let text = "短内容";
    assert_eq!(truncate_from_head(text, 800), text);
}

/// 单条超预算 → 仍保留最近尾部（不产生空注入）。
#[test]
fn truncate_from_head_single_line_over_budget() {
    let text = "这是一条非常长的消息内容，远超预算";
    let out = truncate_from_head(text, 8);
    assert!(!out.is_empty());
    let body = out.split('\n').next_back().unwrap_or("");
    assert!(body.chars().count() <= 8);
    assert!(body.contains("预算"), "应保留末尾字符");
}
