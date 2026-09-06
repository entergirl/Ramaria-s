//! crates/ramaria-memory/src/event/extractor/tests.rs - L2 事件抽取器单元测试
//!
//! 设计特点:
//! - 覆盖事件 JSON 解析、字段归一化、去重指纹、关系提取与响应分派等纯逻辑。
//! - 使用合成事件与 mock LLM/存储，不依赖真实 LLM/embedding。
use super::*;
use ramaria_core::types::Presentation;
use ramaria_core::types::now_ms;
use uuid::Uuid;

// ---- format_l1_from_cluster ----

#[test]
fn format_single_l1_from_cluster() {
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "测试摘要".into(),
        keywords: vec![
            ramaria_core::keyword::KeywordToken::new("测试").unwrap(),
            ramaria_core::keyword::KeywordToken::new("摘要").unwrap(),
        ],
        embedding: None,
        evidence_notes: vec![],
        salience: 0.5,
        created_at: 1_700_000_000_000,
    };
    let cluster = TopicCluster::new(vec![item]);
    let formatted = EventExtractor::format_l1_from_cluster(&cluster);
    assert!(formatted.contains("[1]"));
    assert!(formatted.contains("测试摘要"));
    assert!(formatted.contains("keywords"));
}

#[test]
fn format_multiple_l1_from_cluster() {
    let item1 = L1Item {
        id: Uuid::new_v4(),
        summary: "第一条摘要".into(),
        keywords: vec![],
        embedding: None,
        evidence_notes: vec![],
        salience: 0.5,
        created_at: now_ms(),
    };
    let item2 = L1Item {
        id: Uuid::new_v4(),
        summary: "第二条摘要".into(),
        keywords: vec![ramaria_core::keyword::KeywordToken::new("kw").unwrap()],
        embedding: None,
        evidence_notes: vec![],
        salience: 0.5,
        created_at: now_ms(),
    };
    let cluster = TopicCluster::new(vec![item1, item2]);
    let formatted = EventExtractor::format_l1_from_cluster(&cluster);
    assert!(formatted.contains("[1]"));
    assert!(formatted.contains("[2]"));
    assert!(formatted.contains("第一条摘要"));
    assert!(formatted.contains("第二条摘要"));
}
/// v1.4 M4：evidence_notes 非空时格式化输出 `[线索]` 行，
/// cause 因果线索槽位随行注入（仅供 L2 背景参考）。
#[test]
fn format_l1_with_evidence_notes_injects_clue_lines() {
    use ramaria_core::types::EvidenceNote;
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "用户讨论项目延期安排".into(),
        keywords: vec![],
        embedding: None,
        evidence_notes: vec![EvidenceNote {
            text: "用户提到项目延期到月底".into(),
            time: Some("上周三".into()),
            who: Some("用户".into()),
            cause: Some("需求变更频繁".into()),
        }],
        salience: 0.5,
        created_at: now_ms(),
    };
    let cluster = TopicCluster::new(vec![item]);
    let formatted = EventExtractor::format_l1_from_cluster(&cluster);
    assert!(formatted.contains("[线索]"), "应输出线索行标记");
    assert!(formatted.contains("用户提到项目延期到月底"), "应含证据文本");
    assert!(
        formatted.contains("cause: 需求变更频繁"),
        "应注入 cause 槽位"
    );
    assert!(formatted.contains("time: 上周三"), "应注入 time 槽位");
    assert!(formatted.contains("who: 用户"), "应注入 who 槽位");
}

/// v1.4 M4：线索的缺失槽位不输出占位（仅 text 的线索只输出文本本身）。
#[test]
fn format_l1_evidence_notes_omits_missing_slots() {
    use ramaria_core::types::EvidenceNote;
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "仅文本线索摘要".into(),
        keywords: vec![],
        embedding: None,
        evidence_notes: vec![EvidenceNote::new("用户提到通勤时间变长")],
        salience: 0.5,
        created_at: now_ms(),
    };
    let cluster = TopicCluster::new(vec![item]);
    let formatted = EventExtractor::format_l1_from_cluster(&cluster);
    assert!(formatted.contains("[线索] 用户提到通勤时间变长"));
    assert!(!formatted.contains("time:"), "缺失槽位不应输出占位");
    assert!(!formatted.contains("who:"));
    assert!(!formatted.contains("cause:"));
}

/// v1.4 M4：无证据线索时输出与 v1.3 完全一致（不产生空线索行，回归保护）。
#[test]
fn format_l1_without_evidence_notes_unchanged() {
    let item = L1Item {
        id: Uuid::new_v4(),
        summary: "无证据摘要".into(),
        keywords: vec![ramaria_core::keyword::KeywordToken::new("kw").unwrap()],
        embedding: None,
        evidence_notes: vec![],
        salience: 0.5,
        created_at: now_ms(),
    };
    let cluster = TopicCluster::new(vec![item]);
    let formatted = EventExtractor::format_l1_from_cluster(&cluster);
    assert!(formatted.contains("[1]"));
    assert!(formatted.contains("无证据摘要"));
    assert!(formatted.contains("(keywords: kw)"));
    assert!(!formatted.contains("[线索]"), "无线索时不应出现线索行");
}

// ---- parse_event_response ----

#[test]
fn parse_valid_event_array() {
    let raw = r#"[
            {"title": "跳槽", "summary": "用户换了新工作", "confidence": 0.9, "salience": 0.75}
        ]"#;
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].title.as_deref(), Some("跳槽"));
    assert!(result.relations.is_none());
}

#[test]
fn parse_empty_array() {
    let raw = "[]";
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert!(result.events.is_empty());
}

#[test]
fn parse_single_object_wrapped() {
    // LLM 有时返回单对象而非数组
    let raw = r#"{"title": "事件", "summary": "描述", "confidence": 0.8, "salience": 0.5}"#;
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
}

#[test]
fn parse_with_think_tags() {
    let raw = "<think>analyzing</think>\n[{\"title\": \"测试\", \"summary\": \"摘要\", \"confidence\": 0.7}]";
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
}

#[test]
fn parse_with_prefix_text() {
    let raw =
        "以下是提取的事件：\n[{\"title\": \"事件\", \"summary\": \"描述\", \"confidence\": 0.8}]";
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
}

#[test]
fn parse_invalid_json_returns_error() {
    let raw = "这不是JSON";
    assert!(EventExtractor::parse_event_response(raw).is_err());
}

// ---- 新格式解析 ----

#[test]
fn parse_v13_format_with_events_and_relations() {
    let raw = r#"{
            "events": [
                {"title": "跳槽", "summary": "换工作", "confidence": 0.9, "motives": ["自主"]},
                {"title": "失眠", "summary": "工作压力失眠", "confidence": 0.85}
            ],
            "relations": [
                {"from_index": 0, "to_index": 1, "kind": "CausedBy", "weight": 0.8, "detail": "跳槽导致压力"}
            ]
        }"#;
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 2);
    assert_eq!(result.events[0].title.as_deref(), Some("跳槽"));
    assert!(result.relations.is_some());
    let rels = result.relations.unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].kind, "CausedBy");
    assert_eq!(rels[0].from_index, 0);
    assert_eq!(rels[0].to_index, 1);
}

#[test]
fn parse_v13_format_without_relations() {
    let raw = r#"{
            "events": [
                {"title": "事件", "summary": "描述", "confidence": 0.7, "motives": ["归属"]}
            ]
        }"#;
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
    // relations 字段缺失 → None
    assert!(result.relations.is_none());
}

#[test]
fn parse_v13_format_empty_relations() {
    let raw = r#"{
            "events": [
                {"title": "事件", "summary": "描述", "confidence": 0.7}
            ],
            "relations": []
        }"#;
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
    // 空 relations 数组 → Some([])
    assert!(result.relations.is_some());
    assert!(result.relations.unwrap().is_empty());
}

#[test]
fn parse_v13_format_with_prefix_text() {
    // LLM 可能在 JSON 对象前加前缀文字
    let raw = "以下是提取的结果：\n{\"events\": [{\"title\": \"事件\", \"summary\": \"描述\", \"confidence\": 0.8}]}";
    let result = EventExtractor::parse_event_response(raw).unwrap();
    assert_eq!(result.events.len(), 1);
}

// ---- parse_relation_kind ----

/// parse_relation_kind 各类型参数化验证（未知类型回退 RelatedTo）。
#[test]
fn parse_relation_kind_cases() {
    use ramaria_core::types::EventRelationKind;
    let cases = [
        ("CausedBy", EventRelationKind::CausedBy),
        ("PartOf", EventRelationKind::PartOf),
        ("RelatedTo", EventRelationKind::RelatedTo),
        ("ContinuedBy", EventRelationKind::ContinuedBy),
        ("Contradicts", EventRelationKind::Contradicts),
        ("Timeline", EventRelationKind::Timeline),
        ("UnknownType", EventRelationKind::RelatedTo), // 未知 → 默认 RelatedTo
    ];
    for (input, expected) in cases {
        assert_eq!(parse_relation_kind(input), expected, "input={input:?}");
    }
}

// ---- build_event ----

#[test]
fn build_event_with_all_fields() {
    let json = ExtractedEventJson {
        title: Some("跳槽".into()),
        summary: Some("用户换了新工作".into()),
        keywords: Some("工作, 跳槽, 职业".into()),
        participants: Some(serde_json::json!(["老板", "同事"])),
        confidence: Some(0.9),
        salience: Some(0.75),
        valence: Some(-0.5),
        presentation: Some("subjective".into()),
        share: Some(0.3),
        attitude: Some("既兴奋又不安".into()),
        motives: Some(vec!["自主".to_string(), "地位".to_string()]),
    };
    let now = now_ms();
    let event = EventExtractor::build_event("user-0001", json, now - 1000, now, now, None);

    assert_eq!(event.title, "跳槽");
    assert_eq!(event.persona_uid, "user-0001");
    assert!((event.confidence - 0.9).abs() < f64::EPSILON);
    assert_eq!(event.presentation, Presentation::Subjective);
    assert!(event.situation_strength.is_none());
    assert!(event.attitude.is_some());
    assert_eq!(event.motives.as_deref(), Some("自主,地位"));
}

#[test]
fn build_event_with_empty_motives() {
    let json = ExtractedEventJson {
        title: Some("事件".into()),
        summary: Some("描述".into()),
        keywords: None,
        participants: None,
        confidence: None,
        salience: None,
        valence: None,
        presentation: None,
        share: None,
        attitude: None,
        motives: Some(vec!["".to_string(), "  ".to_string()]),
    };
    let now = now_ms();
    let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
    // 空字符串被过滤 → motives 为 None
    assert!(event.motives.is_none());
}

#[test]
fn build_event_with_none_motives() {
    let json = ExtractedEventJson {
        title: Some("事件".into()),
        summary: Some("描述".into()),
        keywords: None,
        participants: None,
        confidence: None,
        salience: None,
        valence: None,
        presentation: None,
        share: None,
        attitude: None,
        motives: None,
    };
    let now = now_ms();
    let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
    assert!(event.motives.is_none());
}

#[test]
fn build_event_long_title_truncation() {
    let json = ExtractedEventJson {
        title: Some("这是一个超过二十个字的非常长的标题需要截断处理".into()),
        summary: Some("描述".into()),
        keywords: None,
        participants: None,
        confidence: None,
        salience: None,
        valence: None,
        presentation: None,
        share: None,
        attitude: None,
        motives: None,
    };
    let now = now_ms();
    let event = EventExtractor::build_event("user-0001", json, now, now, now, None);
    assert!(event.title.chars().count() <= 20);
}

#[test]
fn build_event_defaults() {
    let json = ExtractedEventJson {
        title: Some("事件".into()),
        summary: Some("描述".into()),
        keywords: None,
        participants: None,
        confidence: None,
        salience: None,
        valence: None,
        presentation: None,
        share: None,
        attitude: None,
        motives: None,
    };
    let now = now_ms();
    let event = EventExtractor::build_event("user-0001", json, now, now, now, None);

    assert!((event.confidence - 0.5).abs() < f64::EPSILON);
    assert!((event.salience - 0.5).abs() < f64::EPSILON);
    assert!((event.valence - 0.0).abs() < f64::EPSILON);
    assert_eq!(event.presentation, Presentation::Mixed);
    assert!(event.situation_strength.is_none());
    assert_eq!(event.share, 0.5);
}

// ---- 钳制函数（与 utils.rs 自身测试重复，已删除） ----

// ---- timestamp_to_date_str ----

/// timestamp_to_date_str 各时间戳参数化验证。
#[test]
fn timestamp_to_date_cases() {
    let cases = [(0i64, "1970-01-01"), (1_748_736_000_000i64, "2025-06-01")];
    for (ts, expected) in cases {
        assert_eq!(timestamp_to_date_str(ts), expected, "ts={ts}");
    }
}

// ---- extract_first_json_array ----

#[test]
fn extract_array_simple() {
    let text = r#"前缀 [{"a":1}, {"b":2}] 后缀"#;
    let result = crate::utils::extract_first_json_array(text).unwrap();
    assert!(result.starts_with('['));
    assert!(result.ends_with(']'));
}

#[test]
fn extract_array_no_brackets() {
    assert!(crate::utils::extract_first_json_array("no array here").is_none());
}

// ---- EventResponse::into_result ----

#[test]
fn response_array_to_result() {
    let resp = EventResponse::Array(vec![ExtractedEventJson {
        title: Some("测试".into()),
        summary: Some("摘要".into()),
        keywords: None,
        participants: None,
        confidence: Some(0.8),
        salience: Some(0.5),
        valence: Some(0.0),
        presentation: None,
        share: None,
        attitude: None,
        motives: None,
    }]);
    let result = resp.into_result().unwrap();
    assert_eq!(result.events.len(), 1);
    assert!(result.relations.is_none());
}

// ---- L2 聚类去重指纹（v1.5 三层生成缓存 C）----

/// 构造测试用 L1（仅指纹/相似度计算需要的字段有值）。
fn l1_for_fp(id: &str, summary: &str) -> MemoryL1 {
    MemoryL1 {
        id: Uuid::parse_str(id).expect("合法 UUID"),
        session_id: Uuid::new_v4(),
        summary: summary.to_string(),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: now_ms(),
        last_accessed_at: None,
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    }
}

#[test]
fn fingerprint_is_deterministic_and_order_independent() {
    let a = l1_for_fp("00000000-0000-0000-0000-000000000001", "摘要 A");
    let b = l1_for_fp("00000000-0000-0000-0000-000000000002", "摘要 B");
    let c = l1_for_fp("00000000-0000-0000-0000-000000000003", "摘要 C");

    // 同集合（不同读取顺序）→ 同指纹
    let fp_abc = compute_l1_set_fingerprint(&[a.clone(), b.clone(), c.clone()]);
    let fp_cba = compute_l1_set_fingerprint(&[c.clone(), b.clone(), a.clone()]);
    assert_eq!(fp_abc, fp_cba, "集合指纹应与读取顺序无关");
    assert_eq!(fp_abc.len(), 64, "SHA-256 hex 应为 64 字符");

    // 集合变化（移除/新增 L1）→ 指纹变化（自动触发重聚类）
    let fp_ab = compute_l1_set_fingerprint(&[a, b]);
    assert_ne!(fp_abc, fp_ab, "移除 L1 后指纹应变化");
}

#[test]
fn fingerprint_ignores_l1_content_but_is_stable_across_calls() {
    // 指纹仅由 L1 id 决定（隐私：不含摘要原文）；
    // 同一 id 集合即使摘要文本变化，指纹也稳定（保证重跑同集合稳定跳过）。
    let id = "00000000-0000-0000-0000-00000000000a";
    let fp1 = compute_l1_set_fingerprint(&[l1_for_fp(id, "第一次摘要")]);
    let fp2 = compute_l1_set_fingerprint(&[l1_for_fp(id, "重新生成的摘要")]);
    assert_eq!(fp1, fp2);
}

/// 构造测试用事件（title/summary/keywords 为相似度输入）。
fn event_for_sim(title: &str, summary: &str, keywords: Option<&str>) -> MemoryEvent {
    MemoryEvent {
        id: 0,
        persona_uid: "p1".into(),
        title: title.into(),
        summary: summary.into(),
        keywords: keywords.map(|s| s.to_string()),
        participants: None,
        start: 0,
        end: 0,
        confidence: 0.5,
        salience: 0.5,
        valence: 0.0,
        presentation: Presentation::Mixed,
        share: 0.5,
        attitude: None,
        paraphrase: None,
        absorbed: 0,
        situation_strength: None,
        motives: None,
        created_at: 0,
        last_accessed_at: None,
        indexed_at: None,
        index_version: None,
    }
}

#[test]
fn event_similarity_identical_text_is_high() {
    let a = event_for_sim(
        "项目延期",
        "用户表示项目延期到月底，原因是需求频繁变更",
        Some("项目,延期,需求"),
    );
    let b = event_for_sim(
        "项目延期",
        "用户表示项目延期到月底，原因是需求频繁变更",
        Some("项目,延期,需求"),
    );
    let sim = event_text_similarity(&a, &b);
    assert!(sim >= 0.99, "完全相同事件相似度应接近 1.0，实际 {sim}");
}

#[test]
fn event_similarity_rerun_slight_wording_diff_hits_keyword_channel() {
    // 重跑场景：标题/摘要措辞有差异，但关键词一致 → 关键词通道保证判重
    let a = event_for_sim(
        "项目延期",
        "用户说项目要延期到月底，因为需求总在变",
        Some("项目,延期,需求"),
    );
    let b = event_for_sim(
        "项目延期",
        "用户表示项目延期至月底，由于需求频繁变更",
        Some("项目,延期,需求"),
    );
    let sim = event_text_similarity(&a, &b);
    assert!(sim >= 0.95, "关键词一致应判重，实际 {sim}");
}

#[test]
fn event_similarity_unrelated_events_are_low() {
    let a = event_for_sim(
        "周末爬山",
        "用户周末和朋友去爬山，天气很好",
        Some("爬山,周末,天气"),
    );
    let b = event_for_sim("项目延期", "用户表示项目延期到月底", Some("项目,延期,需求"));
    let sim = event_text_similarity(&a, &b);
    assert!(sim < 0.95, "无关事件相似度应低于阈值，实际 {sim}");
}

#[test]
fn event_similarity_missing_keywords_falls_back_to_text() {
    // 关键词缺失 → 纯文本 bigram 通道兜底
    let a = event_for_sim("加班到深夜", "用户连续加班到深夜，身体疲惫", None);
    let b = event_for_sim("加班到深夜", "用户连续加班到深夜，身体非常疲惫", None);
    let sim = event_text_similarity(&a, &b);
    assert!(sim > 0.7, "文本高度相似应通过 bigram 通道判高，实际 {sim}");
    assert!(sim < 1.0, "存在措辞差异，不应完全相等");
}

#[test]
fn event_similarity_empty_text_is_zero() {
    let a = event_for_sim("", "", None);
    let b = event_for_sim("", "", None);
    assert_eq!(event_text_similarity(&a, &b), 0.0, "空文本不应误判为相似");
}
