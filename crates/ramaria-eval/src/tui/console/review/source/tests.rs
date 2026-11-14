//! crates/ramaria-eval/src/tui/console/review/source/tests.rs - Ramaria 原对话片段测试
//!
//! 设计特点:
//! - 合成两段 session 的样本，只断言命中区间与证据行标记
//! - 路径用例只验证拼接规则与语言校验，不读真实语料
//! - 空证据、证据不命中、样本缺失各一条

use super::*;

use chrono::NaiveDate;
use ramaria_service::ChatRole;

use crate::corpus::{Message, Session};

// =========================================================
// 夹具
// =========================================================

fn message(speaker: &str, dia_id: &str, text: &str) -> Message {
    Message {
        role: ChatRole::User,
        speaker: speaker.to_string(),
        dia_id: dia_id.to_string(),
        text: text.to_string(),
    }
}

fn session(index: u32, day: u32, messages: Vec<Message>) -> Session {
    Session {
        index,
        date: NaiveDate::from_ymd_opt(2025, 1, day).expect("日期应合法"),
        messages,
    }
}

fn distant_sample() -> Sample {
    let messages = (1..=9)
        .map(|index| {
            message(
                "Alice",
                &format!("2025-01-02_chat{index}"),
                &format!("消息{index}"),
            )
        })
        .collect();
    Sample {
        sample_id: "sample_alice".to_string(),
        user_key: "alice".to_string(),
        speaker_a: "Alice".to_string(),
        speaker_b: "Bob".to_string(),
        sessions: vec![session(1, 2, messages)],
        qa: Vec::new(),
    }
}

fn sample() -> Sample {
    Sample {
        sample_id: "sample_alice".to_string(),
        user_key: "alice".to_string(),
        speaker_a: "Alice".to_string(),
        speaker_b: "Bob".to_string(),
        sessions: vec![
            session(
                1,
                2,
                vec![
                    message("Alice", "2025-01-02_chat1", "消息一"),
                    message("Bob", "2025-01-02_chat2", "消息二"),
                    message("Alice", "2025-01-02_chat3", "消息三"),
                    message("Bob", "2025-01-02_chat4", "消息四"),
                    message("Alice", "2025-01-02_chat5", "消息五"),
                ],
            ),
            session(
                2,
                3,
                vec![
                    message("Alice", "2025-01-03_chat6", "另一天"),
                    message("Bob", "2025-01-03_chat7", "收尾"),
                ],
            ),
        ],
        qa: Vec::new(),
    }
}

// =========================================================
// 用例
// =========================================================

/// 证据命中中间一条：输出前后各两条，只有证据行高亮。
#[test]
fn snippet_keeps_two_messages_around_evidence() {
    let lines = build_snippet(&[sample()], "alice", &["chat3".to_string()]).expect("片段应可构建");
    assert_eq!(lines.len(), 6, "应有一条 session 头加五条消息");
    assert!(lines[0].text.contains("session 1"));
    assert!(lines[0].text.contains("2025-01-02"));
    assert!(lines[1].text.contains("消息一"));
    assert!(lines[3].highlight, "证据行应高亮");
    assert!(lines[3].text.starts_with(">> "), "证据行应有前缀");
    assert!(lines[3].text.contains("消息三"));
    assert_eq!(
        lines.iter().filter(|line| line.highlight).count(),
        1,
        "只有一个证据行"
    );
    assert!(
        !lines.iter().any(|line| line.text.contains("另一天")),
        "未命中的 session 不出现"
    );
    assert!(lines[5].text.starts_with("   "), "普通行保留说话人格式");
}

/// 两个 session 各命中一次时各一个区间，跨 session 不插省略行。
#[test]
fn snippet_renders_each_hit_session() {
    let lines = build_snippet(
        &[sample()],
        "alice",
        &["chat1".to_string(), "chat7".to_string()],
    )
    .expect("片段应可构建");
    let headers = lines
        .iter()
        .filter(|line| line.text.starts_with("——"))
        .count();
    assert_eq!(headers, 2, "两个 session 各一个区间");
    assert_eq!(
        lines.iter().filter(|line| line.highlight).count(),
        2,
        "两条证据各一行"
    );
    assert_eq!(
        lines.iter().filter(|line| line.text == "…").count(),
        0,
        "不同 session 不插省略行"
    );
}

/// 同一 session 内相邻证据合并为一个区间。
#[test]
fn snippet_merges_adjacent_evidence() {
    let lines = build_snippet(
        &[distant_sample()],
        "alice",
        &["chat2".to_string(), "chat4".to_string()],
    )
    .expect("片段应可构建");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.text.starts_with("—— session 1"))
            .count(),
        1,
        "相邻证据只画一个区间"
    );
    assert!(
        !lines.iter().any(|line| line.text == "…"),
        "同一区间内不插省略"
    );
    assert_eq!(
        lines.iter().filter(|line| line.highlight).count(),
        2,
        "两条证据各一行"
    );
}

/// 同一 session 内远距离证据拆成两块，中间消息不输出。
#[test]
fn snippet_splits_distant_blocks_in_one_session() {
    let lines = build_snippet(
        &[distant_sample()],
        "alice",
        &["chat1".to_string(), "chat9".to_string()],
    )
    .expect("片段应可构建");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.text.starts_with("—— session 1"))
            .count(),
        2,
        "两块各自带 session 头"
    );
    assert_eq!(
        lines.iter().filter(|line| line.text == "…").count(),
        1,
        "两块之间应有一行省略"
    );
    assert_eq!(
        lines.iter().filter(|line| line.highlight).count(),
        2,
        "两条证据各一行"
    );
    assert!(
        !lines.iter().any(|line| line.text.contains("消息五")),
        "块外消息不输出"
    );
}

/// 空证据给不可回答说明。
#[test]
fn snippet_without_evidence_is_explained() {
    let lines = build_snippet(&[sample()], "alice", &[]).expect("片段应可构建");
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].highlight);
    assert!(lines[0].text.contains("不可回答"));
}

/// 证据不在样本中给提示。
#[test]
fn snippet_with_unknown_evidence_is_explained() {
    let lines = build_snippet(&[sample()], "alice", &["chat99".to_string()]).expect("片段应可构建");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].text.contains("证据不在该样本原对话中"));
}

/// 样本缺失返回错误。
#[test]
fn snippet_without_sample_returns_error() {
    let error =
        build_snippet(&[sample()], "bob", &["chat3".to_string()]).expect_err("缺样本应报错");
    assert!(error.contains("bob"), "错误应带样本键: {error}");
}

/// 语料路径按名 / 版本 / 语言拼接。
#[test]
fn corpus_path_uses_registry_file_name() {
    let request = CorpusRequest {
        root: PathBuf::from("work"),
        name: "lifebench".to_string(),
        version: "2.0".to_string(),
        language: "zh".to_string(),
    };
    assert_eq!(request.key(), "lifebench/2.0/zh");
    let path = corpus_path(&request).expect("路径应可拼");
    assert_eq!(
        path,
        PathBuf::from("work")
            .join(".eval")
            .join("corpus")
            .join("lifebench")
            .join("2.0")
            .join(CorpusLanguage::Zh.file_name())
    );

    let broken = CorpusRequest {
        language: "fr".to_string(),
        ..request
    };
    assert!(corpus_path(&broken).is_err(), "未知语言应报错");
}
