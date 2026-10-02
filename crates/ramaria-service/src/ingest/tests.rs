//! crates/ramaria-service/src/ingest/tests.rs - Ramaria 回流写入用例测试
//!
//! 设计特点:
//! - 由 ingest.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖后缀跳过 / 指纹去重 /
//!   会话解析与惰性封存 / 封存许可门禁 / 最终封存与失败保留 / 边界与并发写入六条路径
//! - 真实 SQLite（临时文件库 + 全量 migration）与 mock LLM：
//!   直接断言会话归属、消息落库、去重与封存结果
//! - 并发回流以两个共享同一库文件的引擎驱动：验证写入路径不跨 await 持锁
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::*;
use crate::test_support::{
    L1_JSON_REPLY, MockLlm, engine_on_existing_db, engine_with_db, engine_with_l1_reply,
    seed_channel_session, seed_persona,
};
use crate::types::ChatRole;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;

#[test]
fn suffix_match_skips_resent_prefix() {
    let history = vec![
        key(ramaria_core::types::MessageRole::User, "你好"),
        key(ramaria_core::types::MessageRole::Assistant, "你好呀"),
    ];
    let turns = vec![
        ChatTurn {
            role: ChatRole::User,
            content: "你好".to_string(),
        },
        ChatTurn {
            role: ChatRole::Assistant,
            content: "你好呀".to_string(),
        },
        ChatTurn {
            role: ChatRole::User,
            content: "今天聊聊工作".to_string(),
        },
    ];
    assert_eq!(suffix_match_len(&history, &turns), 2, "已入库前缀应被跳过");

    // 完全无关 → 不跳过
    let fresh = vec![ChatTurn {
        role: ChatRole::User,
        content: "全新内容".to_string(),
    }];
    assert_eq!(suffix_match_len(&history, &fresh), 0);
}

/// 构造去重键（内容按写入口径 trim）。
fn key(role: ramaria_core::types::MessageRole, content: &str) -> MessageKey {
    MessageKey {
        role,
        content: content.trim().to_string(),
    }
}

#[test]
fn occurrence_counts_tracks_duplicates() {
    let history = vec![
        key(ramaria_core::types::MessageRole::User, "嗯"),
        key(ramaria_core::types::MessageRole::User, "嗯"),
    ];
    let counts = occurrence_counts(&history);
    assert_eq!(
        counts.get(&("user".to_string(), "嗯".to_string())),
        Some(&2),
        "重复内容的出现次数应被累计"
    );
}

#[test]
fn fingerprint_is_stable_and_ordinal_sensitive() {
    let a = ingest_fingerprint("client-A", "user", "你好", 1);
    let b = ingest_fingerprint("client-A", "user", "你好", 1);
    let c = ingest_fingerprint("client-A", "user", "你好", 2);
    let d = ingest_fingerprint("client-B", "user", "你好", 1);
    assert_eq!(a, b, "同输入指纹稳定（重复提交幂等）");
    assert_ne!(a, c, "序数不同 → 指纹不同（支持重复内容）");
    assert_ne!(a, d, "对话标识不同 → 指纹不同（跨对话隔离）");
    assert_eq!(a.len(), 16, "指纹为 16 位 hex");
}

#[test]
fn channel_and_ref_normalization() {
    assert_eq!(normalize_channel("  "), CHANNEL_MCP);
    assert_eq!(normalize_channel(" mcp "), "mcp");
    assert_eq!(normalize_external_ref(Some("  ")), None);
    assert_eq!(
        normalize_external_ref(Some(" client-A ")),
        Some("client-A".to_string())
    );
}

fn turn(role: ChatRole, content: &str) -> ChatTurn {
    ChatTurn {
        role,
        content: content.to_string(),
    }
}

/// 首次写入：新建带通道会话 + 落库 + 通道字段正确。
#[tokio::test]
async fn ingest_creates_channel_session_and_writes() {
    let (engine, storage, dir) = engine_with_db("ingest").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "你好，最近怎么样"),
                turn(ChatRole::Assistant, "挺好的，你呢"),
            ],
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("回流成功");

    assert_eq!(outcome.written, 2);
    assert_eq!(outcome.deduplicated, 0);
    assert!(!outcome.finalized);

    // 会话带通道标识
    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("查询成功")
        .expect("会话应存在");
    assert_eq!(session.channel, CHANNEL_MCP);
    assert_eq!(session.external_ref.as_deref(), Some("client-A"));

    // 消息落库且带归属
    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("查询消息成功");
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|m| m.persona_uid.as_deref() == Some(DEFAULT_PERSONA_UID)),
        "回流消息应带 persona 归属"
    );
    assert!(messages.iter().all(|m| m.fingerprint.is_some()));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重复提交整段对话：前缀跳过 + 新消息写入（幂等）。
#[tokio::test]
async fn ingest_resubmit_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("ingest-idempotent").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let base = IngestRequest {
        messages: vec![
            turn(ChatRole::User, "第一句"),
            turn(ChatRole::Assistant, "第一句回复"),
        ],
        persona: None,
        conversation_id: Some("client-A".to_string()),
        channel: CHANNEL_MCP.to_string(),
        finalize: false,
    };
    engine.ingest(base).await.expect("首次回流成功");

    // 原样重发 → 全部跳过
    let again = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "第一句"),
                turn(ChatRole::Assistant, "第一句回复"),
            ],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("重发成功");
    assert_eq!(again.written, 0, "重发不应新增消息");
    assert_eq!(again.deduplicated, 2, "前缀两条计入去重");

    // 追加新消息 → 只写新增
    let appended = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "第一句"),
                turn(ChatRole::Assistant, "第一句回复"),
                turn(ChatRole::User, "第二句"),
            ],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("追加成功");
    assert_eq!(appended.written, 1);
    assert_eq!(appended.deduplicated, 2);

    let messages = storage
        .list_messages(appended.session_id)
        .await
        .expect("查询消息成功");
    assert_eq!(messages.len(), 3, "库中应共 3 条（无重复）");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一对话内重复内容（如"嗯"）各自入库（指纹序数区分）。
#[tokio::test]
async fn ingest_keeps_repeated_identical_messages() {
    let (engine, storage, dir) = engine_with_db("ingest-repeat").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "嗯"),
                turn(ChatRole::User, "嗯"),
                turn(ChatRole::User, "嗯"),
            ],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("回流成功");

    assert_eq!(outcome.written, 3, "重复内容应各自写入（序数区分指纹）");
    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("查询消息成功");
    assert_eq!(messages.len(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 单流退化：无 conversation_id 时按通道无标识流续写（同一会话）。
#[tokio::test]
async fn ingest_without_conversation_id_reuses_stream() {
    let (engine, storage, dir) = engine_with_db("ingest-null-ref").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let first = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "第一句")],
            persona: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("首次回流成功");
    let second = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "第一句"),
                turn(ChatRole::User, "第二句"),
            ],
            persona: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("二次回流成功");

    assert_eq!(second.session_id, first.session_id, "无标识应续写同一会话");
    assert_eq!(second.written, 1, "仅新增第二句");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 超时另起：活跃会话空闲超阈值 → 先封存（生成 L1）再新建会话写入。
#[tokio::test]
async fn ingest_seals_stale_session_and_starts_new_one() {
    let (engine, storage, dir) = engine_with_l1_reply("ingest-stale", L1_JSON_REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    // 20 分钟前的活跃会话（阈值默认 10 分钟）
    let stale = seed_channel_session(
        &storage,
        DEFAULT_PERSONA_UID,
        CHANNEL_MCP,
        Some("client-A"),
        2,
        now_ms() - 20 * 60_000,
    )
    .await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "新的一段对话")],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("回流成功");

    assert_ne!(outcome.session_id, stale, "超时会话应封存后另起新会话");
    assert_eq!(outcome.written, 1, "新消息应写入新会话");
    let old = storage
        .get_session(stale)
        .await
        .expect("查询成功")
        .expect("旧会话应存在");
    assert!(old.ended_at.is_some(), "旧会话应被关闭");
    assert_eq!(
        storage
            .list_memory_l1(stale)
            .await
            .expect("读取 L1 成功")
            .len(),
        1,
        "旧会话封存应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存门禁（一致化）：续写超时会话时，许可关闭则续写原会话
/// （不封存、不另起、不生成 L1）；恢复许可后按既有口径先封存再另起。
#[tokio::test]
async fn ingest_lazy_seal_is_gated_by_seal_policy() {
    let (engine, storage, dir) = engine_with_l1_reply("ingest-stale-gated", L1_JSON_REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    // 20 分钟前的活跃会话（阈值默认 10 分钟）：无门禁时会被惰性封存
    let stale = seed_channel_session(
        &storage,
        DEFAULT_PERSONA_UID,
        CHANNEL_MCP,
        Some("client-A"),
        2,
        now_ms() - 20 * 60_000,
    )
    .await;

    engine.set_seal_allowed(false);
    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "封存关闭期间的新消息")],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("回流成功");

    assert_eq!(
        outcome.session_id, stale,
        "封存关闭时应续写原会话（不另起）"
    );
    let active = storage
        .get_session(stale)
        .await
        .expect("查询成功")
        .expect("会话应存在");
    assert!(active.ended_at.is_none(), "封存关闭时会话应保持活跃");
    assert!(
        storage
            .list_memory_l1(stale)
            .await
            .expect("读取 L1 成功")
            .is_empty(),
        "封存关闭时不得生成 L1（不消耗 LLM）"
    );
    assert_eq!(
        storage
            .list_messages(stale)
            .await
            .expect("查询消息成功")
            .len(),
        3,
        "新消息应写入原会话"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 封存门禁：`finalize=true` 在许可关闭时只写不封存（会话保持活跃、无 L1）；
/// 恢复许可后同一对话再次 finalize 正常封存（服务层门禁为唯一裁决点）。
#[tokio::test]
async fn ingest_finalize_is_gated_by_seal_policy() {
    let (engine, storage, dir) = engine_with_l1_reply("ingest-finalize-gated", L1_JSON_REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    engine.set_seal_allowed(false);
    let first = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "第一段")],
            persona: None,
            conversation_id: Some("cb-gated".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("回流成功");
    assert!(!first.finalized, "封存关闭时 finalize 应被跳过");
    let session = storage
        .get_session(first.session_id)
        .await
        .expect("查询成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_none(), "会话应保持活跃");
    assert!(
        storage
            .list_memory_l1(first.session_id)
            .await
            .expect("读取 L1 成功")
            .is_empty(),
        "封存关闭时不应生成 L1"
    );

    engine.set_seal_allowed(true);
    let second = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "第二段")],
            persona: None,
            conversation_id: Some("cb-gated".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("回流成功");
    assert_eq!(second.session_id, first.session_id, "未超时应续写同一会话");
    assert!(second.finalized, "恢复许可后 finalize 应完成封存");
    assert_eq!(
        storage
            .list_memory_l1(second.session_id)
            .await
            .expect("读取 L1 成功")
            .len(),
        1,
        "恢复许可后应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 惰性封存失败（LLM 不可用）不阻塞回流：仍另起新会话并写入消息。
#[tokio::test]
async fn ingest_survives_lazy_seal_failure() {
    // 空回复 mock → L1 生成失败
    let (engine, storage, dir) = engine_with_db("ingest-stale-fail").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let stale = seed_channel_session(
        &storage,
        DEFAULT_PERSONA_UID,
        CHANNEL_MCP,
        Some("client-A"),
        2,
        now_ms() - 20 * 60_000,
    )
    .await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "封存失败也要写进来")],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("LLM 不可用不应让回流失败");

    assert_eq!(outcome.written, 1, "消息应已落库");
    assert_ne!(outcome.session_id, stale, "应另起新会话");
    let old = storage
        .get_session(stale)
        .await
        .expect("查询成功")
        .expect("旧会话应存在");
    assert!(old.ended_at.is_some(), "旧会话仍应被关闭");
    assert!(
        storage
            .list_memory_l1(stale)
            .await
            .expect("读取 L1 成功")
            .is_empty(),
        "LLM 不可用时不生成 L1（待补扫）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 长对话（>500 条、内容高重复）整段重发：去重依据覆盖全量，不因窗口截断重复写入。
#[tokio::test]
async fn ingest_long_conversation_resubmit_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("ingest-long").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    // 600 条、仅 3 种不同内容：指纹序数一旦错位就会重复写入（回归窗口截断缺陷）
    let turns: Vec<ChatTurn> = (0..600)
        .map(|i| {
            let content = format!("第 {} 条消息", i % 3);
            turn(ChatRole::User, &content)
        })
        .collect();

    let first = engine
        .ingest(IngestRequest {
            messages: turns.clone(),
            persona: None,
            conversation_id: Some("client-long".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("首次回流成功");
    assert_eq!(first.written, 600);

    let again = engine
        .ingest(IngestRequest {
            messages: turns,
            persona: None,
            conversation_id: Some("client-long".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("重发成功");

    assert_eq!(again.written, 0, "长对话重发不得重复写入");
    assert_eq!(again.deduplicated, 600, "全部应计入去重");
    assert_eq!(
        storage
            .count_messages(again.session_id)
            .await
            .expect("计数成功"),
        600,
        "库中消息数不应增长"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 归属人格不存在 → Validation 错误（先于建会话检查，避免留下空会话）。
#[tokio::test]
async fn ingest_rejects_unknown_persona() {
    let (engine, _storage, dir) = engine_with_db("ingest-unknown").await;

    let err = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "你好")],
            persona: Some("char-9999".to_string()),
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect_err("人格不存在应报错");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 边界：空 messages → Validation 错误；越权人格 → Privacy 错误。
#[tokio::test]
async fn ingest_boundaries_are_explicit() {
    let (engine, _storage, dir) = engine_with_db("ingest-bounds").await;

    let err = engine
        .ingest(IngestRequest {
            messages: Vec::new(),
            persona: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect_err("空 messages 应报错");
    assert_eq!(err.category(), "validation");

    engine.set_recall_policy(
        crate::recall::RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );
    let err = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "你好")],
            persona: Some("char-0002".to_string()),
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect_err("越权人格应报错");
    assert_eq!(err.category(), "privacy");

    let _ = std::fs::remove_dir_all(&dir);
}

/// finalize=true 且 LLM 可用：写入后立即封存并生成 L1。
#[tokio::test]
async fn ingest_finalize_seals_when_llm_available() {
    let (engine, storage, dir) =
        crate::test_support::engine_with_l1_reply("ingest-finalize", L1_JSON_REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "今天加班到很晚"),
                turn(ChatRole::Assistant, "辛苦了，早点休息"),
            ],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("回流成功");

    assert!(outcome.finalized, "LLM 可用时 finalize 应完成封存");
    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_some(), "finalize 应关闭会话");
    assert_eq!(
        storage
            .list_memory_l1(outcome.session_id)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "finalize 应生成 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// finalize=true 但 LLM 不可用：消息写入成功，`finalized=false`（摘要留给空闲/补扫）。
#[tokio::test]
async fn ingest_finalize_failure_keeps_written_data() {
    let (engine, storage, dir) = engine_with_db("ingest-finalize-fail").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "今天加班到很晚")],
            persona: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: true,
        })
        .await
        .expect("封存失败不应让回流整体失败");

    assert_eq!(outcome.written, 1, "消息应已落库");
    assert!(!outcome.finalized, "摘要未生成时 finalized 应为 false");
    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("查询会话应成功")
        .expect("会话应存在");
    assert!(session.ended_at.is_some(), "会话仍应被关闭（不阻塞新会话）");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空内容消息跳过（不写脏数据，也不计入去重）。
#[tokio::test]
async fn ingest_skips_blank_content() {
    let (engine, storage, dir) = engine_with_db("ingest-blank").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let outcome = engine
        .ingest(IngestRequest {
            messages: vec![
                turn(ChatRole::User, "   "),
                turn(ChatRole::User, "有效内容"),
            ],
            persona: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        })
        .await
        .expect("回流成功");

    assert_eq!(outcome.written, 1);
    assert_eq!(outcome.deduplicated, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 多宿主并发写（服务层等价物）：两台引擎各自回流 → 均成功且无写锁报错。
#[tokio::test]
async fn concurrent_ingest_from_two_engines_succeeds() {
    let (engine_a, storage, dir) = engine_with_db("ingest-concurrent").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    // 第二台引擎：同一库文件、独立连接池（模拟"桌面 + MCP"并存写同一库）
    let engine_b = engine_on_existing_db(
        &dir.join("assistant.db"),
        MockLlm::local(),
        RamariaConfig::default(),
    )
    .await;

    let (first, second) = tokio::join!(
        engine_a.ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "客户端 A 的第一句")],
            persona: None,
            conversation_id: Some("client-A".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        }),
        engine_b.ingest(IngestRequest {
            messages: vec![turn(ChatRole::User, "客户端 B 的第一句")],
            persona: None,
            conversation_id: Some("client-B".to_string()),
            channel: CHANNEL_MCP.to_string(),
            finalize: false,
        }),
    );

    let outcome_a = first.expect("客户端 A 回流不应因写锁失败");
    let outcome_b = second.expect("客户端 B 回流不应因写锁失败");
    assert_eq!(outcome_a.written, 1);
    assert_eq!(outcome_b.written, 1);
    assert_ne!(
        outcome_a.session_id, outcome_b.session_id,
        "不同外部对话标识应各自成会话"
    );
    // 两个会话均在库中可见（桌面可见回流内容的前提）
    assert_eq!(
        storage
            .count_messages(outcome_a.session_id)
            .await
            .expect("统计消息应成功"),
        1
    );
    assert_eq!(
        storage
            .count_messages(outcome_b.session_id)
            .await
            .expect("统计消息应成功"),
        1
    );

    let _ = std::fs::remove_dir_all(&dir);
}
