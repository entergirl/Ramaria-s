//! crates/ramaria-memory/src/l1/summarizer/tests/evidence.rs - evidence_notes 校验
//!
//! 设计特点:
//! - 由 父测试模块 以 mod evidence; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// evidence_notes 校验测试
// =========================================================

#[test]
fn evidence_notes_valid_list_is_preserved() {
    // 正常产出证据片段 → 保留全部有效条目
    let notes = vec![
        EvidenceNote::new("用户表示最近一个月每天加班到10点以后"),
        EvidenceNote::new("用户说'感觉身体被掏空了'"),
        EvidenceNote::new("用户提到'周末也经常被叫去开会'"),
    ];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert_eq!(result.len(), 3);
    assert!(result[0].text.contains("加班"));
}

#[test]
fn evidence_notes_null_downgrades_to_empty() {
    // LLM 未输出 evidence_notes → 降级为空数组
    let result = validate_evidence_notes(None, Uuid::new_v4());
    assert!(result.is_empty(), "evidence_notes 为 None 时应降级为空数组");
}

#[test]
fn evidence_notes_empty_array_downgrades_to_empty() {
    // LLM 输出空数组 → 降级为空数组
    let result = validate_evidence_notes(Some(vec![]), Uuid::new_v4());
    assert!(result.is_empty(), "evidence_notes 为空数组时应降级为空数组");
}

#[test]
fn evidence_notes_short_items_are_filtered() {
    // 过短条目（< 5 字符）应被丢弃
    let notes = vec![
        EvidenceNote::new("太长的一条完整证据描述文本"),
        EvidenceNote::new("短"), // < 5 字符，应丢弃
        EvidenceNote::new("OK"), // < 5 字符，应丢弃
        EvidenceNote::new("足够长的证据描述文本内容"),
    ];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert_eq!(result.len(), 2);
    assert!(result[0].text.contains("太长"));
    assert!(result[1].text.contains("足够"));
}

#[test]
fn evidence_notes_all_short_downgrades_to_empty() {
    // 全部条目过短 → 降级为空数组
    let notes = vec![
        EvidenceNote::new("短"),
        EvidenceNote::new("A"),
        EvidenceNote::new("B"),
    ];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert!(result.is_empty(), "全部 evidence 过短时应降级为空数组");
}

#[test]
fn evidence_notes_parse_from_valid_json() {
    // JSON 解析：包含 evidence_notes 数组（旧字符串数组 → 宽容转换为对象）
    let raw = r#"{
            "summary": "测试",
            "valence": 0.0,
            "salience": 0.5,
            "evidence_notes": ["证据一：用户提到项目延期", "证据二：用户表示压力很大"]
        }"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    let notes = parsed.evidence_notes.unwrap();
    assert_eq!(notes.len(), 2);
    assert!(notes[0].text.contains("项目延期"));
}

#[test]
fn evidence_notes_parse_structured_object_array() {
    // JSON 解析：对象数组（v1.4 新格式）直接解析为结构化 EvidenceNote
    let raw = r#"{
            "summary": "测试",
            "valence": 0.0,
            "salience": 0.5,
            "evidence_notes": [
                {"text": "用户提到项目延期", "time": "上周三", "who": "用户", "cause": "需求变更"}
            ]
        }"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    let notes = parsed.evidence_notes.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "用户提到项目延期");
    assert_eq!(notes[0].time.as_deref(), Some("上周三"));
    assert_eq!(notes[0].who.as_deref(), Some("用户"));
    assert_eq!(notes[0].cause.as_deref(), Some("需求变更"));
}

#[test]
fn evidence_notes_parse_mixed_items() {
    // JSON 解析：混合旧字符串与对象条目 → 全部转换为 EvidenceNote
    let raw = r#"{
            "summary": "测试",
            "evidence_notes": ["旧格式字符串", {"text": "新格式对象"}]
        }"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    let notes = parsed.evidence_notes.unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].text, "旧格式字符串");
    assert!(notes[0].time.is_none());
    assert_eq!(notes[1].text, "新格式对象");
}

#[test]
fn evidence_notes_parse_null_array_defaults_none() {
    // JSON 中 evidence_notes 为 null → 返回 None（降级路径）
    let raw = r#"{"summary": "测试", "evidence_notes": null}"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    assert!(parsed.evidence_notes.is_none());
}

#[test]
fn evidence_notes_parse_missing_field_defaults_none() {
    // JSON 缺失 evidence_notes 字段 → serde(default) 应返回 None
    let raw = r#"{"summary": "测试", "valence": 0.0, "salience": 0.5}"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    assert!(parsed.evidence_notes.is_none());
}

#[test]
fn validate_and_build_evidence_notes_present() {
    // validate_and_build 整合测试：正常 evidence_notes 应保留
    let parsed = L1SummaryResponse {
        summary: Some("测试摘要".into()),
        keywords: None,
        time_period: Some("上午".into()),
        atmosphere: Some("专注".into()),
        valence: Some(0.0),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: Some(vec![EvidenceNote::new("用户提到项目截止日期临近")]),
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
    let notes = l1.evidence_notes.expect("evidence_notes 不应为 None");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].text.contains("项目截止日期"));
}

#[test]
fn validate_and_build_evidence_notes_missing_downgrades() {
    // validate_and_build 整合测试：缺失 evidence_notes 降级为空数组
    let parsed = L1SummaryResponse {
        summary: Some("测试摘要".into()),
        keywords: None,
        time_period: Some("上午".into()),
        atmosphere: Some("轻松".into()),
        valence: Some(0.5),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
    let notes = l1
        .evidence_notes
        .expect("evidence_notes 不应为 None，应为 Some(vec![])");
    assert!(notes.is_empty(), "缺失 evidence_notes 时应降级为空数组");
}

// ---- 结构化槽位校验测试 ----

/// 完整对象（text + time/who/cause 全部槽位）经校验后槽位完整保留。
#[test]
fn evidence_notes_full_object_slots_preserved() {
    let notes = vec![EvidenceNote {
        text: "用户提到项目延期到月底".into(),
        time: Some("上周三".into()),
        who: Some("用户".into()),
        cause: Some("需求变更频繁".into()),
    }];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].text, "用户提到项目延期到月底");
    assert_eq!(result[0].time.as_deref(), Some("上周三"));
    assert_eq!(result[0].who.as_deref(), Some("用户"));
    assert_eq!(result[0].cause.as_deref(), Some("需求变更频繁"));
}

/// 可选槽位为空字符串或纯空白 → 归一为 None（缺省即无，不阻塞生成）。
#[test]
fn evidence_notes_blank_optional_slots_normalized_to_none() {
    let notes = vec![EvidenceNote {
        text: "用户表示最近压力很大".into(),
        time: Some("".into()),   // 空字符串
        who: Some("   ".into()), // 纯空白
        cause: Some("".into()),  // 空字符串
    }];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert_eq!(result.len(), 1, "text 有效时条目应保留");
    assert!(result[0].time.is_none(), "空 time 应归一为 None");
    assert!(result[0].who.is_none(), "空白 who 应归一为 None");
    assert!(result[0].cause.is_none(), "空 cause 应归一为 None");
}

/// 可选槽位带首尾空白 → trim 后保留有效内容。
#[test]
fn evidence_notes_optional_slots_are_trimmed() {
    let notes = vec![EvidenceNote {
        text: "用户提到通勤时间变长".into(),
        time: Some(" 上周五 ".into()),
        who: Some(" 同事 ".into()),
        cause: Some(" 搬家 ".into()),
    }];
    let result = validate_evidence_notes(Some(notes), Uuid::new_v4());
    assert_eq!(result[0].time.as_deref(), Some("上周五"));
    assert_eq!(result[0].who.as_deref(), Some("同事"));
    assert_eq!(result[0].cause.as_deref(), Some("搬家"));
}

/// 反序列化：对象条目缺少 text（如 text 为数字等非法类型）→ 跳过该条并记 warn，
/// 其余合法条目保留（解析失败不阻塞整体）。
#[test]
fn evidence_notes_parse_invalid_object_item_skipped() {
    let raw = r#"{
            "summary": "测试",
            "evidence_notes": [
                {"text": 123, "cause": "非法类型"},
                {"text": "用户提到项目顺利上线"}
            ]
        }"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    let notes = parsed.evidence_notes.expect("应产出部分有效条目");
    assert_eq!(notes.len(), 1, "非法条目应被跳过，合法条目保留");
    assert_eq!(notes[0].text, "用户提到项目顺利上线");
}

/// 反序列化：非字符串非对象的非法条目（数字/布尔）→ 跳过该条。
#[test]
fn evidence_notes_parse_non_object_items_skipped() {
    let raw = r#"{
            "summary": "测试",
            "evidence_notes": [42, true, "用户提到天气转凉"]
        }"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    let notes = parsed.evidence_notes.expect("应产出部分有效条目");
    assert_eq!(notes.len(), 1, "数字/布尔条目应被跳过");
    assert_eq!(notes[0].text, "用户提到天气转凉");
}
