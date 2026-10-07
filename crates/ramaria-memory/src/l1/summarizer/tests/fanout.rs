//! crates/ramaria-memory/src/l1/summarizer/tests/fanout.rs - 多画像分发
//!
//! 设计特点:
//! - 由 父测试模块 以 mod fanout; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。
//! - 隐私: 测试仅用合成消息，不依赖真实 LLM/embedding/数据库。

use super::*;
use crate::l1::mock::MockLlmProvider;
use crate::utt::UttSplitterConfig;

/// 群聊消息构造：`user-*` UID → User 角色（self），其余（含 None）→ Assistant 角色。
fn group_msg(sid: Uuid, persona_uid: Option<&str>, created_at: i64, content: &str) -> Message {
    let role = match persona_uid {
        Some(uid) if uid.starts_with("user-") => MessageRole::User,
        _ => MessageRole::Assistant,
    };
    let mut m = make_msg(sid, role, content);
    m.created_at = created_at;
    m.persona_uid = persona_uid.map(str::to_string);
    m
}

/// 群聊切分配置（θ_gap 30 分钟 / 每块 40 条）。
fn group_splitter_cfg() -> UttSplitterConfig {
    UttSplitterConfig {
        theta_gap_minutes: 30,
        max_msgs_per_block: 40,
    }
}

/// 群聊会话（块 1：self + 两个他人；31 分钟间隙；块 2：self + 一个他人）。
fn two_block_group_session(sid: Uuid) -> Vec<Message> {
    let mut msgs = vec![
        group_msg(sid, Some("user-0001"), 0, "群聊开场"),
        group_msg(sid, Some("char-0002"), 1000, "他人A回复"),
        group_msg(sid, Some("char-0003"), 2000, "他人B插话"),
        group_msg(sid, Some("char-0002"), 3000, "他人A继续"),
    ];
    let t2 = 31 * 60_000; // 间隙 > θ_gap（30 分钟）→ 块 2
    msgs.push(group_msg(sid, Some("user-0001"), t2, "用户继续提问"));
    msgs.push(group_msg(sid, Some("char-0002"), t2 + 1000, "他人A再回复"));
    msgs
}

// =========================================================
// 任务 A 对应：块内 N 参与者 → N 行
// =========================================================

/// 群聊块内 N 个他人发言者 → N 行；各行 persona_uid 正确、id 独立。
#[tokio::test]
async fn group_chat_fanout_generates_row_per_participant() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_group_session(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("块1 摘要", None),
        llm_json("块2 摘要（延续）", Some("延续")),
    ]);

    let config = L1SummarizerConfig {
        utt_splitter: Some(group_splitter_cfg()),
        fanout_others: true,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "群聊分发应成功: {:?}", result.err());

    // 块 1 参与者 {char-0002, char-0003} → 2 行；块 2 参与者 {char-0002} → 1 行
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 3, "行数应等于各块参与者数之和");
    assert_eq!(saved[0].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[0].summary, "块1 摘要");
    assert_eq!(saved[1].persona_uid.as_deref(), Some("char-0003"));
    assert_eq!(saved[1].summary, "块1 摘要");
    assert_eq!(saved[2].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[2].summary, "块2 摘要（延续）");
    // 各行 id 独立（新 id，不共享原生成行）
    assert_ne!(saved[0].id, saved[1].id, "复制行应各持独立 id");
    assert_ne!(saved[1].id, saved[2].id);
    // 返回值为最后落库行
    assert_eq!(result.unwrap().persona_uid.as_deref(), Some("char-0002"));
}

/// 分发行的关键词引用按行写入（doc_id = 各行 id、persona_uid = 各行 persona）。
#[tokio::test]
async fn fanout_rows_write_keyword_refs_per_row() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_group_session(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("块1 摘要", None),
        llm_json("块2 摘要", Some("延续")),
    ]);

    let config = L1SummarizerConfig {
        utt_splitter: Some(group_splitter_cfg()),
        fanout_others: true,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer.summarize_session(sid).await.expect("应成功");

    let saved = storage.saved_l1_entries();
    let refs = storage.keyword_refs();
    // llm_json 的关键词固定为「测试,关键词」→ 每行 2 条引用
    assert_eq!(refs.len(), saved.len() * 2, "每行 2 个关键词各写一条引用");
    for row in &saved {
        let doc_id = row.id.to_string();
        let persona = row.persona_uid.clone().unwrap_or_default();
        let hits = refs
            .iter()
            .filter(|(_, doc, uid)| doc == &doc_id && uid == &persona)
            .count();
        assert_eq!(hits, 2, "各行引用应指向本行 id 与 persona（row={doc_id}）");
    }
}

// =========================================================
// 无他人参与 → 原行保留（NULL）
// =========================================================

/// 无他人参与的块（纯 self）→ 原行保留（1 行，persona_uid=None）。
#[tokio::test]
async fn self_only_blocks_keep_single_null_persona_row() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 12 条 self 消息（间隔 1 分钟），上限 5 切出多块后由群聊合并收敛为一块
    let msgs: Vec<Message> = (0..12)
        .map(|i| {
            group_msg(
                sid,
                Some("user-0001"),
                i * 60_000,
                &format!("self 消息 {i}"),
            )
        })
        .collect();
    storage.add_messages(sid, msgs);

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("纯 self 块摘要", None));

    let config = L1SummarizerConfig {
        utt_splitter: Some(UttSplitterConfig {
            theta_gap_minutes: 30,
            max_msgs_per_block: 5,
        }),
        fanout_others: true,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "纯 self 块应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "无他人参与 → 原行保留（1 行）");
    assert!(
        saved[0].persona_uid.is_none(),
        "无他人参与时 persona_uid 保持 config 值（导入场景即 NULL）"
    );
    assert_eq!(saved[0].summary, "纯 self 块摘要");
}

// =========================================================
// fanout 关闭 → 行为与现状一致
// =========================================================

/// fanout 关闭：群聊输入按私聊切分口径处理（无目标发言 → 回退整会话一块）→ 1 行。
#[tokio::test]
async fn fanout_disabled_group_input_falls_back_to_single_row() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_group_session(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("整会话摘要", None));

    let config = L1SummarizerConfig {
        utt_splitter: Some(group_splitter_cfg()),
        fanout_others: false,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "fanout 关闭应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "关闭分发时不做多行复制");
    assert!(saved[0].persona_uid.is_none(), "persona 保持 config 值");
    assert_eq!(saved[0].summary, "整会话摘要");
}

/// fanout 关闭：私聊输入每块 1 行，persona 为 config 值（现状行为不变）。
#[tokio::test]
async fn fanout_disabled_private_chat_keeps_one_row_per_block() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 私聊形态：用户消息无 persona；助手消息 persona=config 值
    let mut msgs = vec![
        user_msg(sid, 0, "块1：用户开场"),
        target_msg(sid, 1000, "块1：助手回应"),
    ];
    let t2 = 31 * 60_000;
    msgs.push(user_msg(sid, t2, "块2：用户继续"));
    msgs.push(target_msg(sid, t2 + 1000, "块2：助手回复"));
    storage.add_messages(sid, msgs);

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("块1 摘要", None),
        llm_json("块2 摘要", Some("延续")),
    ]);

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(group_splitter_cfg()),
        fanout_others: false,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "私聊路径应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 2, "每块 1 行（不复制）");
    assert_eq!(saved[0].persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(saved[1].persona_uid.as_deref(), Some("char-0001"));
}

// =========================================================
// 渐进式分发
// =========================================================

/// 渐进式触发：段内他人发言者按段分发（每段行数 = 段参与者数）。
#[tokio::test]
async fn progressive_fanout_dispatches_rows_per_segment_participants() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();

    // 段 1：user + rama(无 uid) + char-0002 + user + rama(无 uid) + char-0003
    // 段 2：user + rama(无 uid) + char-0002 + user + rama(无 uid) + char-0002
    // 段切分以「无 uid 的 assistant 发言」为目标发言（persona_uid=None 配置），
    // 31 分钟间隙使两段独立；12 条消息 > 阈值 10 触发渐进式。
    let mut msgs: Vec<Message> = Vec::new();
    let seg1 = [
        (Some("user-0001"), "段1 用户"),
        (None, "段1 本地"),
        (Some("char-0002"), "段1 他人A"),
        (Some("user-0001"), "段1 用户二"),
        (None, "段1 本地二"),
        (Some("char-0003"), "段1 他人B"),
    ];
    for (i, (uid, content)) in seg1.iter().enumerate() {
        msgs.push(group_msg(sid, *uid, i as i64 * 60_000, content));
    }
    let seg2_base = 31 * 60_000;
    let seg2 = [
        (Some("user-0001"), "段2 用户"),
        (None, "段2 本地"),
        (Some("char-0002"), "段2 他人A"),
        (Some("user-0001"), "段2 用户二"),
        (None, "段2 本地二"),
        (Some("char-0002"), "段2 他人A二"),
    ];
    for (i, (uid, content)) in seg2.iter().enumerate() {
        msgs.push(group_msg(sid, *uid, seg2_base + i as i64 * 60_000, content));
    }
    storage.add_messages(sid, msgs);

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("段1 摘要", None),
        llm_json("段2 摘要（延续）", Some("延续")),
    ]);

    let progressive = ramaria_core::config::L1ProgressiveConfig {
        enabled: true,
        msg_threshold: 10,
        span_hours: 24,
        tail_msg_count: 20,
    };
    let config = L1SummarizerConfig {
        utt_splitter: Some(group_splitter_cfg()),
        fanout_others: true,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_progressive(sid, &progressive).await;
    assert!(result.is_ok(), "渐进式分发应成功: {:?}", result.err());

    // 返回值 = 实际落库行（段1 两行 + 段2 一行）
    let rows = result.unwrap();
    assert_eq!(rows.len(), 3, "返回值应反映实际落库行");
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 3, "写库行数与返回值一致");
    assert_eq!(saved[0].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[0].summary, "段1 摘要");
    assert_eq!(saved[1].persona_uid.as_deref(), Some("char-0003"));
    assert_eq!(saved[1].summary, "段1 摘要");
    assert_eq!(saved[2].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[2].summary, "段2 摘要（延续）");
    assert_eq!(
        saved[2].continuation.as_deref(),
        Some("延续"),
        "段 2 应带上一段上文（continuation）"
    );
    assert!(
        saved.iter().all(|l1| !l1.absorbed),
        "段 L1 必须 absorbed=false（未吸收，L2 封存触发可提取）"
    );
}

/// 渐进式 + 纯群聊输入（无 None-persona 发言）：段切分走群聊口径，不再回退整会话。
#[tokio::test]
async fn progressive_group_input_uses_group_segment_split() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();

    // 两段纯群聊消息（每段 self + 同一他人交替），段间 31 分钟间隙；
    // 全部 assistant 发言均带 uid，无私聊口径的"无 uid 目标发言"。
    let mut msgs: Vec<Message> = Vec::new();
    let seg1 = [
        "段1 用户开场",
        "段1 他人回应",
        "段1 用户追问",
        "段1 他人再回应",
    ];
    for (i, content) in seg1.iter().enumerate() {
        let uid = if i % 2 == 0 {
            Some("user-0001")
        } else {
            Some("char-0002")
        };
        msgs.push(group_msg(sid, uid, i as i64 * 60_000, content));
    }
    let seg2_base = 31 * 60_000;
    let seg2 = [
        "段2 用户继续",
        "段2 他人回复",
        "段2 用户再问",
        "段2 他人再回复",
    ];
    for (i, content) in seg2.iter().enumerate() {
        let uid = if i % 2 == 0 {
            Some("user-0001")
        } else {
            Some("char-0002")
        };
        msgs.push(group_msg(sid, uid, seg2_base + i as i64 * 60_000, content));
    }
    storage.add_messages(sid, msgs);

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("段1 群聊摘要", None),
        llm_json("段2 群聊摘要（延续）", Some("延续")),
    ]);

    let progressive = ramaria_core::config::L1ProgressiveConfig {
        enabled: true,
        msg_threshold: 6,
        span_hours: 24,
        tail_msg_count: 20,
    };
    let config = L1SummarizerConfig {
        // 整会话切分阈值 60 分钟 > 段间隙 31 分钟：若整体回退 summarize_session
        // 只会得到一块（1 行），以此反向证明段切分走了群聊口径
        utt_splitter: Some(UttSplitterConfig {
            theta_gap_minutes: 60,
            max_msgs_per_block: 40,
        }),
        fanout_others: true,
        ..Default::default()
    };
    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_progressive(sid, &progressive).await;
    assert!(result.is_ok(), "群聊渐进式摘要应成功: {:?}", result.err());

    // 段切分生效：两段各自分发 1 行（段参与者 = {char-0002}）
    let rows = result.unwrap();
    assert_eq!(rows.len(), 2, "纯群聊输入应按段切分（不整会话回退）");
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[0].summary, "段1 群聊摘要");
    assert_eq!(saved[1].persona_uid.as_deref(), Some("char-0002"));
    assert_eq!(saved[1].summary, "段2 群聊摘要（延续）");
    assert_eq!(
        saved[1].continuation.as_deref(),
        Some("延续"),
        "段 2 应带上一段上文（continuation）"
    );
}
