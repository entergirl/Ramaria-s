//! crates/ramaria-memory/src/l1/summarizer/tests/projection.rs - L1 目标相对投影
//!
//! 设计特点:
//! - 由 父测试模块 以 mod projection; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 覆盖逐目标生成（N 目标 N 行）/ 逐目标角色重投影（prompt 断言）/ 发言人渲染与
//!   退化 / 目标数上限截断 / 关闭时回归锚点。
//! - 用例为确定性断言，可离线运行。
//! - 隐私: 测试仅用合成消息，不依赖真实 LLM/embedding/数据库。

use super::*;
use crate::l1::mock::MockLlmProvider;

// =========================================================
// 夹具
// =========================================================

/// 社会通道消息：指定发言人人格、存储角色与显示名。
fn speaker_msg(
    sid: Uuid,
    persona_uid: &str,
    role: MessageRole,
    sender_name: Option<&str>,
    created_at: i64,
    content: &str,
) -> Message {
    let mut m = make_msg(sid, role, content);
    m.created_at = created_at;
    m.persona_uid = Some(persona_uid.to_string());
    m.sender_name = sender_name.map(str::to_string);
    m
}

/// 投影配置：绑定人格 + 目标相对投影开启（切分与上限走默认值）。
fn projection_config(bound_persona: &str) -> L1SummarizerConfig {
    L1SummarizerConfig {
        persona_uid: Some(bound_persona.to_string()),
        target_projection: true,
        ..Default::default()
    }
}

/// 双发言者单块会话：本机侧（rama-0001）+ 一个成员（char-0001）。
fn two_speaker_block(sid: Uuid) -> Vec<Message> {
    vec![
        speaker_msg(
            sid,
            "rama-0001",
            MessageRole::Assistant,
            Some("小拉"),
            1000,
            "今天好累",
        ),
        speaker_msg(
            sid,
            "char-0001",
            MessageRole::User,
            Some("小明"),
            2000,
            "我也累了",
        ),
    ]
}

// =========================================================
// 逐目标生成：N 目标 N 行
// =========================================================

/// 投影开启：2 目标 → 2 行落库（各自 persona_uid），原行不落（无 None 归属行）。
#[tokio::test]
async fn projection_generates_one_row_per_target() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_speaker_block(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("本机侧视角摘要", None),
        llm_json("成员视角摘要", None),
    ]);

    let summarizer = L1Summarizer::new(&llm, &storage, projection_config("rama-0001"));
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "投影生成应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 2, "2 目标应落 2 行");
    assert_eq!(saved[0].persona_uid.as_deref(), Some("rama-0001"));
    assert_eq!(saved[0].summary, "本机侧视角摘要");
    assert_eq!(saved[1].persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(saved[1].summary, "成员视角摘要");
    assert!(
        saved.iter().all(|row| row.persona_uid.is_some()),
        "投影模式不得出现无归属行"
    );
    // 返回值为最后落库行
    assert_eq!(result.unwrap().persona_uid.as_deref(), Some("char-0001"));
}

// =========================================================
// 逐目标角色重投影与发言人渲染
// =========================================================

/// 每目标一次生成调用；prompt 中角色相对投影、行首渲染发言人。
#[tokio::test]
async fn projection_roles_render_per_target() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_speaker_block(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("本机侧视角摘要", None),
        llm_json("成员视角摘要", None),
    ]);

    let summarizer = L1Summarizer::new(&llm, &storage, projection_config("rama-0001"));
    summarizer.summarize_session(sid).await.expect("应成功");

    let requests = llm.requests();
    assert_eq!(requests.len(), 2, "2 目标应调用 2 次 LLM");

    // target=rama-0001：本机侧为「助手：小拉：」，成员为「用户：小明：」
    let first = &requests[0].user_message;
    assert!(
        first.contains("助手：小拉：今天好累"),
        "本机侧应映射助手前缀并带发言人: {first}"
    );
    assert!(
        first.contains("用户：小明：我也累了"),
        "成员应映射用户前缀并带发言人: {first}"
    );

    // target=char-0001：本机侧翻转为「用户：小拉：」，成员为「助手：小明：」
    let second = &requests[1].user_message;
    assert!(
        second.contains("用户：小拉：今天好累"),
        "本机侧在成员视角应为用户: {second}"
    );
    assert!(
        second.contains("助手：小明：我也累了"),
        "成员在本人视角应为助手: {second}"
    );
}

/// sender_name 缺失 → 行退化为 `{prefix}{content}`。
#[tokio::test]
async fn projection_sender_name_fallback() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        sid,
        vec![
            speaker_msg(
                sid,
                "rama-0001",
                MessageRole::Assistant,
                None,
                1000,
                "今天好累",
            ),
            speaker_msg(
                sid,
                "char-0001",
                MessageRole::User,
                Some("小明"),
                2000,
                "我也累了",
            ),
        ],
    );

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(vec![
        llm_json("本机侧视角摘要", None),
        llm_json("成员视角摘要", None),
    ]);

    let summarizer = L1Summarizer::new(&llm, &storage, projection_config("rama-0001"));
    summarizer.summarize_session(sid).await.expect("应成功");

    let requests = llm.requests();
    let first = &requests[0].user_message;
    assert!(
        first.contains("助手：今天好累"),
        "无显示名时应退化为前缀直连正文: {first}"
    );
    assert!(!first.contains("助手：None"), "不得渲染 None 文本: {first}");
    assert!(first.contains("用户：小明：我也累了"), "有显示名照常渲染");
}

// =========================================================
// 目标数上限截断
// =========================================================

/// 超过上限的发言者：目标 = 8（绑定人格 + 7 个消息数最多的发言者）。
#[tokio::test]
async fn projection_target_cap() {
    let sid = Uuid::new_v4();
    let mut msgs = vec![speaker_msg(
        sid,
        "rama-0001",
        MessageRole::Assistant,
        Some("小拉"),
        1000,
        "本机侧发言",
    )];
    // char-0001 × 3 条、char-0002 × 2 条、char-0003 × 2 条
    for (uid, count) in [("char-0001", 3), ("char-0002", 2), ("char-0003", 2)] {
        for i in 0..count {
            msgs.push(speaker_msg(
                sid,
                uid,
                MessageRole::User,
                Some(uid),
                2000 + msgs.len() as i64 * 10,
                &format!("{uid} 第 {i} 条"),
            ));
        }
    }
    // char-0004 ~ char-0010 各 1 条（多数为 1 条 → 按首现序取 4 个）
    for n in 4..=10 {
        msgs.push(speaker_msg(
            sid,
            &format!("char-{n:04}"),
            MessageRole::User,
            Some(&format!("成员{n}")),
            2000 + msgs.len() as i64 * 10,
            "单条发言",
        ));
    }
    let storage = MockStorage::new();
    storage.add_messages(sid, msgs);

    let llm = MockLlmProvider::new("test-model");
    llm.set_responses(
        (0..8)
            .map(|i| llm_json(&format!("摘要 {i}"), None))
            .collect(),
    );

    let summarizer = L1Summarizer::new(&llm, &storage, projection_config("rama-0001"));
    summarizer.summarize_session(sid).await.expect("应成功");

    let saved = storage.saved_l1_entries();
    assert_eq!(
        saved.len(),
        8,
        "目标数应受上限约束（绑定人格 + 7 个发言者）"
    );
    let personas: Vec<&str> = saved
        .iter()
        .filter_map(|row| row.persona_uid.as_deref())
        .collect();
    assert_eq!(personas[0], "rama-0001", "绑定人格恒在首位");
    // 条数最多者优先入选
    for uid in ["char-0001", "char-0002", "char-0003"] {
        assert!(personas.contains(&uid), "{uid} 应入选（条数较多）");
    }
    // 全部为 1 条的发言者按首现序补足剩余名额（char-0004 首现最早）
    assert!(
        personas.contains(&"char-0004"),
        "首现最早的 1 条发言者应入选"
    );
    assert_eq!(llm.requests().len(), 8, "每目标一次生成调用");
}

// =========================================================
// 关闭时回归锚点
// =========================================================

/// 投影关闭（默认）：单行、prompt 与存储角色口径一致（与现状锚定）。
#[tokio::test]
async fn projection_disabled_unchanged() {
    let sid = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(sid, two_speaker_block(sid));

    let llm = MockLlmProvider::new("test-model");
    llm.set_response(llm_json("回归摘要", None));

    // 默认配置：投影关闭、绑定人格 None（本地路径口径）
    let summarizer = L1Summarizer::new(&llm, &storage, L1SummarizerConfig::default());
    let result = summarizer.summarize_session(sid).await;
    assert!(result.is_ok(), "默认配置应成功: {:?}", result.err());

    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "默认配置应单行");
    assert!(saved[0].persona_uid.is_none(), "未绑定人格时归属保持 None");

    // prompt 保持存储角色口径（无发言人渲染）
    let request = llm.last_request().expect("应有请求");
    let prompt = &request.user_message;
    assert!(
        prompt.contains("助手：今天好累"),
        "存储角色为助手: {prompt}"
    );
    assert!(
        prompt.contains("用户：我也累了"),
        "存储角色为用户: {prompt}"
    );
    assert!(
        !prompt.contains("小拉") && !prompt.contains("小明"),
        "关闭时不应渲染发言人: {prompt}"
    );
    assert_eq!(llm.requests().len(), 1, "关闭时单次生成调用");
}
