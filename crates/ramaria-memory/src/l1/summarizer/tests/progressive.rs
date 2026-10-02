//! crates/ramaria-memory/src/l1/summarizer/tests/progressive.rs - B3 渐进式摘要
//!
//! 设计特点:
//! - 由 父测试模块 以 mod progressive; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// B3 渐进式摘要（v1.7，决策 D-V17-005）
// =========================================================

fn progressive_cfg() -> ramaria_core::config::L1ProgressiveConfig {
    ramaria_core::config::L1ProgressiveConfig {
        enabled: true,
        msg_threshold: 10,
        span_hours: 24,
        tail_msg_count: 5,
    }
}

/// 触发条件（条数边界）：恰好等于阈值不触发，超过阈值触发。
#[test]
fn progressive_trigger_by_count_boundary() {
    let sid = Uuid::new_v4();
    let msgs_10: Vec<Message> = (0..10).map(|i| user_msg(sid, i * 1000, "内容")).collect();
    let cfg = ramaria_core::config::L1ProgressiveConfig {
        msg_threshold: 10,
        ..Default::default()
    };
    assert!(
        !is_progressive_triggered(&msgs_10, &cfg),
        "消息数恰好等于阈值不应触发"
    );

    let msgs_11: Vec<Message> = (0..11).map(|i| user_msg(sid, i * 1000, "内容")).collect();
    assert!(
        is_progressive_triggered(&msgs_11, &cfg),
        "消息数超过阈值应触发"
    );
}

/// 触发条件（时间跨度边界）：跨度恰好等于阈值不触发，超过阈值触发。
#[test]
fn progressive_trigger_by_span_boundary() {
    let sid = Uuid::new_v4();
    let span_23h = 23 * 3600 * 1000;
    let msgs_23h = vec![user_msg(sid, 0, "开头"), user_msg(sid, span_23h, "结尾")];
    let cfg = ramaria_core::config::L1ProgressiveConfig {
        span_hours: 24,
        ..Default::default()
    };
    assert!(
        !is_progressive_triggered(&msgs_23h, &cfg),
        "跨度 23h（≤ 阈值 24h）不应触发"
    );

    let msgs_25h = vec![
        user_msg(sid, 0, "开头"),
        user_msg(sid, 25 * 3600 * 1000, "结尾"),
    ];
    assert!(
        is_progressive_triggered(&msgs_25h, &cfg),
        "跨度 25h（> 阈值 24h）应触发"
    );
}

/// 未启用（enabled=false）→ 委托 summarize_session（v1.6 行为：单条 L1）。
#[tokio::test]
async fn progressive_disabled_falls_back_to_single_l1() {
    use crate::l1::mock::MockLlmProvider;
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 11 条消息（超过阈值 10），但 progressive 未启用 → 不触发
    storage.add_messages(
        sid,
        (0..11)
            .map(|i| user_msg(sid, i * 1000, "长会话内容"))
            .collect(),
    );
    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("整会话摘要", None));

    let cfg = ramaria_core::config::L1ProgressiveConfig {
        enabled: false,
        msg_threshold: 10,
        span_hours: 24,
        tail_msg_count: 5,
    };
    let summarizer = L1Summarizer::new(
        &llm,
        &storage,
        L1SummarizerConfig {
            utt_splitter: None, // 整会话单块，确保断言可控
            persona_uid: Some("char-0001".into()),
            ..Default::default()
        },
    );
    let result = summarizer.summarize_progressive(sid, &cfg).await;
    assert!(result.is_ok(), "未启用应成功: {:?}", result.err());
    let l1_list = result.unwrap();
    assert_eq!(l1_list.len(), 1, "未启用应只生成 1 条 L1");
    assert_eq!(storage.saved_l1_entries().len(), 1, "写库 1 条");
}

/// 触发分段：12 条消息（> 阈值 10），tail=5 → 3 段 L1 全部写库（absorbed=0 入候选池）。
#[tokio::test]
async fn progressive_triggered_generates_multiple_l1_in_candidate_pool() {
    use crate::l1::mock::MockLlmProvider;
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 交替 user/target 消息（块内必须含目标 persona 发言，split_messages 规则 3）；
    // 12 条按 tail=5 切 5+5+2，尾块含双侧发言避免单边合并。
    let msgs: Vec<Message> = (0..12)
        .map(|i| {
            if i % 2 == 0 {
                target_msg(sid, i * 1000, &format!("长会话消息 {i}"))
            } else {
                user_msg(sid, i * 1000, &format!("长会话消息 {i}"))
            }
        })
        .collect();
    storage.add_messages(sid, msgs);
    let llm = MockLlmProvider::new("test-model");
    // 3 段 → 3 次 LLM 调用；第 2/3 段带上一块上文
    llm.set_responses(vec![
        llm_json("段 1 摘要", None),
        llm_json("段 2 摘要", Some("延续")),
        llm_json("段 3 摘要（尾部）", Some("延续")),
    ]);

    let summarizer = L1Summarizer::new(
        &llm,
        &storage,
        L1SummarizerConfig {
            utt_splitter: None,
            persona_uid: Some("char-0001".into()),
            ..Default::default()
        },
    );
    let cfg = progressive_cfg();
    let result = summarizer.summarize_progressive(sid, &cfg).await;
    assert!(result.is_ok(), "触发分段应成功: {:?}", result.err());

    let l1_list = result.unwrap();
    assert_eq!(l1_list.len(), 3, "12 条消息 tail=5 应切 3 段");
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 3, "3 段 L1 全部写库（入候选池）");
    assert!(
        saved.iter().all(|l1| !l1.absorbed),
        "段 L1 必须 absorbed=false（未吸收，L2 封存触发可提取）"
    );
    assert!(
        saved.last().unwrap().summary.contains("尾部"),
        "最后一段应覆盖最新对话（按 tail_msg_count 切段、全段生成）"
    );
    assert!(
        saved.last().unwrap().continuation.is_some(),
        "第 2/3 段带上一块上文 → continuation 非空"
    );
}

/// 未达触发阈值（消息数 ≤ 阈值且跨度 ≤ 阈值）→ 整会话 1 条 L1（v1.6 语义）。
#[tokio::test]
async fn progressive_not_triggered_single_l1() {
    use crate::l1::mock::MockLlmProvider;
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        sid,
        vec![
            user_msg(sid, 0, "短会话消息 1"),
            user_msg(sid, 1000, "短会话消息 2"),
        ],
    );
    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("短会话摘要", None));

    let summarizer = L1Summarizer::new(
        &llm,
        &storage,
        L1SummarizerConfig {
            utt_splitter: None,
            persona_uid: Some("char-0001".into()),
            ..Default::default()
        },
    );
    let cfg = progressive_cfg();
    let result = summarizer.summarize_progressive(sid, &cfg).await;
    assert!(result.is_ok(), "未触发应成功: {:?}", result.err());
    let l1_list = result.unwrap();
    assert_eq!(l1_list.len(), 1, "未触发应整会话 1 条 L1");
    assert_eq!(storage.saved_l1_entries().len(), 1);
}

/// 触发判断的时间跨度按 created_at 极值计算，**不依赖输入排序**：
/// 乱序输入（含时间戳缺失 `created_at=0`）不 panic，且跨度判定与升序输入一致。
#[test]
fn progressive_trigger_uses_min_max_span_and_survives_anomalies() {
    let cfg = ramaria_core::config::L1ProgressiveConfig {
        msg_threshold: 100, // 只测时间跨度分支
        span_hours: 24,
        ..Default::default()
    };

    // 乱序 + 一条 created_at=0（模拟缺失时间戳）：真实跨度 25h（0 与 25h 之间）
    let sid = Uuid::new_v4();
    let msgs_unsorted = vec![
        user_msg(sid, 25 * 3600 * 1000, "最新"),
        user_msg(sid, 0, "缺失时间戳"),
        user_msg(sid, 3600 * 1000, "中间"),
    ];
    assert!(
        is_progressive_triggered(&msgs_unsorted, &cfg),
        "乱序输入按 min/max 跨度应触发（25h > 24h）"
    );

    // 单条 created_at=0：跨度视为 0，不触发时间条件（不 panic）
    let single = vec![user_msg(sid, 0, "仅一条")];
    assert!(
        !is_progressive_triggered(&single, &cfg),
        "单条消息跨度 0 不应触发时间条件"
    );

    // 全部时间戳为 0（导入异常数据）：不 panic，且不按超大跨度误触发
    let zeros: Vec<Message> = (0..3)
        .map(|i| user_msg(sid, 0, &format!("消息{i}")))
        .collect();
    assert!(
        !is_progressive_triggered(&zeros, &cfg),
        "全 0 时间戳不应因时间条件触发"
    );
}

/// 仅由**时间跨度**触发的完整生成路径：跨 25h 的两簇消息按 10min 间隙切为两段，
/// 每段独立生成 absorbed=false 的 L1（按 `tail_msg_count` 切段、全段生成），
/// 尾段覆盖最新对话。
#[tokio::test]
async fn progressive_span_triggered_path_generates_tail_l1s() {
    use crate::l1::mock::MockLlmProvider;
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    // 第一簇 t≈0（前 4 条，交替双侧），第二簇 t≈25h（后 4 条，交替双侧）；
    // 消息数 8 ≤ 阈值 10（不按条数触发），仅跨度 25h > 24h 触发。
    let mut msgs: Vec<Message> = Vec::new();
    for (i, ts) in [0i64, 1000, 2000, 3000].into_iter().enumerate() {
        if i % 2 == 0 {
            msgs.push(target_msg(sid, ts, &format!("早段消息 {i}")));
        } else {
            msgs.push(user_msg(sid, ts, &format!("早段消息 {i}")));
        }
    }
    let tail_base = 25 * 3600 * 1000;
    for (i, ts) in [
        tail_base,
        tail_base + 1000,
        tail_base + 2000,
        tail_base + 3000,
    ]
    .into_iter()
    .enumerate()
    {
        if i % 2 == 0 {
            msgs.push(target_msg(sid, ts, &format!("尾段消息 {i}")));
        } else {
            msgs.push(user_msg(sid, ts, &format!("尾段消息 {i}")));
        }
    }
    storage.add_messages(sid, msgs);
    let llm = MockLlmProvider::new("test-model");
    // 两簇（10min 间隙）→ 2 段 LLM 调用；第 2 段带上一块上文
    llm.set_responses(vec![
        llm_json("早段摘要", None),
        llm_json("尾段摘要（最新）", Some("延续")),
    ]);

    let summarizer = L1Summarizer::new(
        &llm,
        &storage,
        L1SummarizerConfig {
            utt_splitter: None,
            persona_uid: Some("char-0001".into()),
            ..Default::default()
        },
    );
    let cfg = progressive_cfg();
    let result = summarizer.summarize_progressive(sid, &cfg).await;
    assert!(result.is_ok(), "跨度触发应成功: {:?}", result.err());
    let l1_list = result.unwrap();
    assert_eq!(l1_list.len(), 2, "跨 25h 两簇应按时间切 2 段");
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 2, "2 段 L1 全部写库（入候选池）");
    assert!(
        saved.iter().all(|l1| !l1.absorbed),
        "跨度触发的段 L1 也必须 absorbed=false"
    );
    assert!(
        saved.last().unwrap().summary.contains("最新"),
        "尾段应覆盖最新对话"
    );
}

/// 长块且上一 L1 缺失（降级截断）在中文 + emoji 多字节输入下不 panic，
/// 且截断结果 ≤ 预算、处于字符边界（统一 core text 工具保证）。
#[test]
fn prior_context_truncates_mixed_cjk_emoji_on_char_boundary() {
    let sid = Uuid::new_v4();
    let msgs: Vec<Message> = (0..21)
        .map(|i| {
            user_msg(
                sid,
                1000 + i * 1000,
                &format!("长消息第 {i} 段：工作压力很大😀需要休息💤"),
            )
        })
        .collect();
    let chunk = make_chunk(msgs);
    let cfg = L1SummarizerConfig {
        prior_context_max_chars: 60,
        ..Default::default()
    };
    let ctx = build_prior_context(&chunk, None, &cfg, "用户：", "助手：");
    assert!(ctx.ends_with('…'), "应含统一省略号截断标记");
    assert!(
        ctx.chars().count() <= 60,
        "截断后长度受控（预算内含省略号）: {}",
        ctx.chars().count()
    );
    // 不 panic 且非空（字符边界安全）
    assert!(!ctx.is_empty());
}
