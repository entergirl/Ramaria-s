//! crates/ramaria-memory/src/l1/summarizer/tests/utt.rs - B2 上下文感知生成
//!
//! 设计特点:
//! - 由 父测试模块 以 mod utt; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// B2 上下文感知生成测试
// =========================================================

use crate::utt::UttSplitterConfig;

/// 构造带 continuation 的 MemoryL1（供 build_prior_context 测试）。
fn make_l1(summary: &str, notes: Vec<EvidenceNote>) -> MemoryL1 {
    MemoryL1 {
        id: ramaria_core::types::new_id(),
        session_id: ramaria_core::types::new_id(),
        summary: summary.to_string(),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: 0,
        last_accessed_at: None,
        persona_uid: Some("char-0001".to_string()),
        context_json: None,
        situation_strength: None,
        evidence_notes: Some(notes),
        continuation: Some("延续".to_string()),
    }
}

// ---- build_prior_context：两种上文形态 ----

/// 短块（消息数 ≤ 阈值）→ 注入 L0 原文（混合形态之一）。
#[test]
fn prior_context_short_block_injects_raw_text() {
    let sid = Uuid::new_v4();
    let chunk = make_chunk(vec![
        user_msg(sid, 1000, "今天工作好累"),
        target_msg(sid, 2000, "辛苦了，早点休息"),
    ]);
    let cfg = L1SummarizerConfig::default();
    let ctx = build_prior_context(
        &chunk,
        Some(&make_l1("摘要", vec![])),
        &cfg,
        "用户：",
        "助手：",
    );
    // 短块即使有 L1 也注入原文
    assert!(ctx.contains("今天工作好累"), "应注入短块原文");
    assert!(ctx.contains("辛苦了，早点休息"), "应注入短块原文");
    assert!(!ctx.contains("[上一块摘要]"), "短块不应注入摘要形态");
}

/// 长块 + 上一 L1 → 注入摘要 + 结构化线索（含可选槽位）。
#[test]
fn prior_context_long_block_injects_summary_and_notes() {
    let sid = Uuid::new_v4();
    // 21 条消息（> 阈值 20）→ 长块
    let msgs: Vec<Message> = (0..21)
        .map(|i| {
            if i % 2 == 0 {
                target_msg(sid, 1000 + i * 1000, &format!("target 消息 {i}"))
            } else {
                user_msg(sid, 1000 + i * 1000, &format!("user 消息 {i}"))
            }
        })
        .collect();
    let chunk = make_chunk(msgs);
    let prev_l1 = make_l1(
        "用户抱怨项目延期",
        vec![EvidenceNote {
            text: "用户提到项目延期到月底".into(),
            time: Some("上周三".into()),
            who: Some("用户".into()),
            cause: Some("需求变更频繁".into()),
        }],
    );
    let cfg = L1SummarizerConfig::default();
    let ctx = build_prior_context(&chunk, Some(&prev_l1), &cfg, "用户：", "助手：");
    assert!(ctx.contains("[上一块摘要] 用户抱怨项目延期"), "应注入摘要");
    assert!(ctx.contains("用户提到项目延期到月底"), "应注入线索 text");
    assert!(ctx.contains("时间：上周三"), "线索可选槽位应保留");
    assert!(ctx.contains("人物：用户"), "线索 who 槽位应保留");
    assert!(ctx.contains("原因：需求变更频繁"), "线索 cause 槽位应保留");
}

/// 长块 + 上一 L1（无 evidence_notes）→ 仅注入摘要，不报错。
#[test]
fn prior_context_long_block_l1_without_notes_injects_summary_only() {
    let sid = Uuid::new_v4();
    let msgs: Vec<Message> = (0..21)
        .map(|i| user_msg(sid, 1000 + i * 1000, "消息"))
        .collect();
    let chunk = make_chunk(msgs);
    let prev_l1 = make_l1("用户聊了天气", vec![]);
    let cfg = L1SummarizerConfig::default();
    let ctx = build_prior_context(&chunk, Some(&prev_l1), &cfg, "用户：", "助手：");
    assert!(ctx.contains("[上一块摘要] 用户聊了天气"));
    assert!(!ctx.contains("[上一块线索]"), "无线索时不应输出线索段落");
}

/// 长块无上一 L1（降级）→ 注入上一块原文并截断到上限。
#[test]
fn prior_context_long_block_without_l1_truncates_raw() {
    let sid = Uuid::new_v4();
    let msgs: Vec<Message> = (0..21)
        .map(|i| {
            user_msg(
                sid,
                1000 + i * 1000,
                "这是一条足够长的用户消息内容用于截断测试",
            )
        })
        .collect();
    let chunk = make_chunk(msgs);
    let cfg = L1SummarizerConfig {
        prior_context_max_chars: 100,
        ..Default::default()
    };
    let ctx = build_prior_context(&chunk, None, &cfg, "用户：", "助手：");
    assert!(ctx.ends_with('…'), "应含统一省略号截断标记");
    assert!(
        ctx.chars().count() <= 100,
        "截断后长度受控（统一工具预算内含省略号）: {}",
        ctx.chars().count()
    );
}

/// 消息数恰好等于阈值 → 短块形态（原文）；超过阈值 → 长块形态（L1）。
#[test]
fn prior_context_threshold_boundary() {
    let sid = Uuid::new_v4();
    let cfg = L1SummarizerConfig::default();
    // 恰 20 条（= 阈值）→ 原文
    let msgs: Vec<Message> = (0..20)
        .map(|i| user_msg(sid, 1000 + i * 1000, "内容"))
        .collect();
    let chunk = make_chunk(msgs.clone());
    let ctx = build_prior_context(
        &chunk,
        Some(&make_l1("摘要", vec![])),
        &cfg,
        "用户：",
        "助手：",
    );
    assert!(!ctx.contains("[上一块摘要]"), "= 阈值仍为短块原文形态");
    // 21 条（> 阈值）→ L1 摘要形态
    let mut msgs2: Vec<Message> = msgs;
    msgs2.push(user_msg(sid, 1000 + 20 * 1000, "内容"));
    let chunk2 = make_chunk(msgs2);
    let ctx2 = build_prior_context(
        &chunk2,
        Some(&make_l1("摘要", vec![])),
        &cfg,
        "用户：",
        "助手：",
    );
    assert!(ctx2.contains("[上一块摘要]"), "超过阈值应注入 L1 摘要");
}

// ---- validate_continuation ----

/// 三个合法枚举值均保留（trim 后）。
#[test]
fn continuation_valid_values_kept() {
    for (raw, expected) in [("延续", "延续"), (" 转折 ", "转折"), ("无关", "无关")] {
        let v = validate_continuation(Some(raw), Uuid::new_v4());
        assert_eq!(v.as_deref(), Some(expected), "值 {raw} 应保留");
    }
}

/// 非法值 → 置 None 不阻塞。
#[test]
fn continuation_invalid_value_dropped() {
    let v = validate_continuation(Some("延续中"), Uuid::new_v4());
    assert!(v.is_none(), "非法 continuation 应置 None");
    let v2 = validate_continuation(Some("cont"), Uuid::new_v4());
    assert!(v2.is_none());
}

/// 缺失/空白 → None（正常路径）。
#[test]
fn continuation_missing_or_blank_dropped() {
    assert!(validate_continuation(None, Uuid::new_v4()).is_none());
    assert!(validate_continuation(Some("   "), Uuid::new_v4()).is_none());
    assert!(validate_continuation(Some(""), Uuid::new_v4()).is_none());
}

// ---- summarize_session 集成（多块上下文感知） ----

/// 构造一个 2 块的 session：块1 与块2 间隔 > θ_gap（30 分钟）。
fn two_block_session(sid: Uuid) -> Vec<Message> {
    // 块1：2 条（短块），时间 0 ~ 1000
    let mut msgs = vec![
        user_msg(sid, 0, "块1：用户开场"),
        target_msg(sid, 1000, "块1：助手回应"),
    ];
    // 间隙 > 30 分钟（θ_gap=30）→ 块2
    let t2 = 31 * 60_000;
    msgs.push(user_msg(sid, t2, "块2：用户继续提问"));
    msgs.push(target_msg(sid, t2 + 1000, "块2：助手回复"));
    msgs
}

/// 多块 session → 每块生成一条 L1；第二块带 continuation（有上文）。
#[tokio::test]
async fn multi_block_generates_one_l1_per_block_with_continuation() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_session(sid));

    let llm = MockLlmProvider::new("test-model");
    // 块1 无上文 → 无 continuation；块2 有上文 → continuation="延续"
    llm.set_responses(vec![
        llm_json("块1 摘要", None),
        llm_json("块2 摘要（延续上一话题）", Some("延续")),
    ]);

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(UttSplitterConfig {
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
        }),
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "多块生成应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 2, "每块应生成一条 L1");
    assert_eq!(saved[0].summary, "块1 摘要");
    assert!(
        saved[0].continuation.is_none(),
        "首块无上文 → continuation=None"
    );
    assert_eq!(saved[1].summary, "块2 摘要（延续上一话题）");
    assert_eq!(
        saved[1].continuation.as_deref(),
        Some("延续"),
        "第二块应带 continuation"
    );

    // 返回值为最后一块的 L1
    let l1 = result.unwrap();
    assert_eq!(l1.summary, "块2 摘要（延续上一话题）");
}

/// 第二块生成时 prompt 注入上一块原文（短块形态）；只注入最近 1 块。
#[tokio::test]
async fn second_block_prompt_includes_prior_block_raw() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_session(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("块1 摘要", None),
        llm_json("块2 摘要", Some("无关")),
    ]);

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(UttSplitterConfig {
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
        }),
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    summarizer.summarize_session(sid).await.expect("应成功");

    // 最后一次请求 = 块2：prompt 应含块1 原文与 continuation 字段说明
    let last = llm.last_request().expect("应有请求记录");
    assert!(
        last.user_message.contains("块1：用户开场"),
        "应注入块1 原文"
    );
    assert!(
        last.user_message.contains("块1：助手回应"),
        "应注入块1 原文"
    );
    assert!(
        last.user_message.contains("continuation"),
        "带上文模板应含 continuation"
    );
    // 只注入最近 1 块：块2 原文是当前块内容，应出现在块2 的 prompt 对话部分
    //（上文注入的是块1，不含第三块链式内容）
    assert!(
        last.user_message.contains("块2：用户继续提问"),
        "块2 原文是当前块内容，应出现在块2 prompt 中"
    );
}

/// 单块 session（无上一块）→ 与 v1.4 行为一致：一条 L1、continuation=None、
/// prompt 为 v1.4 模板（不含 continuation 字段）。
#[tokio::test]
async fn single_block_session_matches_v1_4_behavior() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        sid,
        vec![
            user_msg(sid, 0, "单块消息"),
            target_msg(sid, 1000, "单块回复"),
        ],
    );

    let llm = MockLlmProvider::new("test-model");
    // LLM 意外输出 continuation → 无上文时强制置 None（保持 v1.4 语义）
    llm.set_response(llm_json("单块摘要", Some("延续")));

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(UttSplitterConfig::default()),
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "单块生成应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "单块只生成一条 L1");
    assert!(
        saved[0].continuation.is_none(),
        "无上一块时 continuation 强制 None"
    );

    // prompt 应使用 v1.4 模板（无 continuation 字段说明）
    let last = llm.last_request().expect("应有请求记录");
    assert!(
        !last.user_message.contains("continuation"),
        "单块无上文时应使用 v1.4 模板"
    );
}

/// 块级失败降级：块1 生成失败 → 块2 仍生成（以上一块原文为上文），不整体失败。
#[tokio::test]
async fn block_failure_degrades_and_later_blocks_continue() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_session(sid));

    let llm = MockLlmProvider::new("test-model");
    // 块1 返回非法 JSON（模拟 LLM 故障）；块2 正常
    llm.set_responses(vec![
        "这不是 JSON".to_string(),
        llm_json("块2 摘要", Some("转折")),
    ]);

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(UttSplitterConfig {
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
        }),
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "块失败应降级继续: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "失败块不写库，成功块照常写库");
    assert_eq!(saved[0].summary, "块2 摘要");
    // 块2 的上文来自块1 原文（短块形态，无需 L1）
    let last = llm.last_request().expect("应有请求记录");
    assert!(
        last.user_message.contains("块1：用户开场"),
        "降级后以块1 原文为上文"
    );
}

/// 全部块失败 → 返回错误（与 v1.4 失败语义一致），无部分写入。
#[tokio::test]
async fn all_blocks_fail_returns_error_no_partial_write() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_block_session(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec!["坏1".to_string(), "坏2".to_string()]);

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: Some(UttSplitterConfig::default()),
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_err(), "全部块失败应返回错误");
    assert!(
        storage.saved_l1_entries().is_empty(),
        "全部失败不应有任何写入"
    );
}

/// 未配置切分器（utt_splitter=None）→ 整会话一块，与 v1.4 完全一致。
#[tokio::test]
async fn no_splitter_config_falls_back_to_v1_4_single_block() {
    use crate::l1::mock::MockLlmProvider;

    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 消息间隔虽大（> θ_gap），但未配置切分器 → 不切块
    storage.add_messages(
        sid,
        vec![
            user_msg(sid, 0, "早上的消息"),
            target_msg(sid, 1000, "早上的回复"),
            user_msg(sid, 2 * 3600 * 1000, "深夜的消息"),
            target_msg(sid, 2 * 3600 * 1000 + 1000, "深夜的回复"),
        ],
    );

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("整会话摘要", None));

    let config = L1SummarizerConfig {
        persona_uid: Some("char-0001".into()),
        utt_splitter: None,
        ..Default::default()
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "未配置切分器应成功: {:?}", result.err());
    assert_eq!(
        storage.saved_l1_entries().len(),
        1,
        "未配置切分器 → 整会话一条 L1"
    );
    // 最后一次（也是唯一一次）请求不含上文
    let last = llm.last_request().expect("应有请求记录");
    assert!(
        !last.user_message.contains("continuation"),
        "v1.4 模板无 continuation"
    );
    assert!(
        last.user_message.contains("早上的消息") && last.user_message.contains("深夜的消息"),
        "整会话消息应全部进入 prompt"
    );
}
