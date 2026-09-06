//! crates/ramaria-memory/src/l1/summarizer/tests.rs - L0-L1 摘要管线单元测试
//!
//! 设计特点:
//! - 覆盖 config 默认值 / JSON 解析 / evidence_notes 宽容校验 / 渐进式触发 / 上文构建 / 集成写库。
//! - LLM 路径使用 mock LlmProvider；存储使用 MockStorage（与 summarizer 主体同 crate 测试夹具）。
//! - 隐私: 测试仅用合成消息，不依赖真实 LLM/embedding/数据库。
use super::*;
use crate::l1::mock::{MockStorage, make_msg};
use ramaria_core::types::{EvidenceNote, Message, MessageRole};

// ---- strip_thinking（与 utils.rs 同名测试完全重复，已删除） ----

/// v1.4 截断修复：默认 max_tokens 应足以容纳含 evidence_notes 的完整 JSON。
///
/// 说明:
/// - 512（Python 旧值）对 v1.4 结构化对象数组输出过紧，LLM 输出易被截断
///   导致 JSON 解析失败；默认值提升至 1024 作为所有未显式传值路径的兜底。
#[test]
fn default_config_max_tokens_sufficient() {
    let cfg = L1SummarizerConfig::default();
    assert_eq!(cfg.max_tokens, 1024, "L1 默认 max_tokens 应为 1024");
    assert!(
        (cfg.temperature - 0.3).abs() < f64::EPSILON,
        "temperature 默认 0.3"
    );
}

// ---- extract_first_json_object ----

#[test]
fn extract_with_markdown_block() {
    let input = "```json\n{\"summary\": \"测试\"}\n```";
    let result = crate::utils::extract_first_json_object(input).unwrap();
    assert!(result.contains("\"summary\""));
}

// ---- clamp_valence（与 utils.rs 同名测试完全重复，已删除） ----

#[test]
fn clamp_valence_boundary() {
    let result = crate::utils::clamp_valence(0.25);
    assert!(result == 0.0 || result == 0.5);
}

// ---- clamp_salience（与 utils.rs 同名测试完全重复，已删除） ----

// ---- validate_and_build (free function) ----

#[test]
fn validate_summary_empty_fallback() {
    let parsed = L1SummaryResponse {
        summary: Some("".into()),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: Some(0.5),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (l1, _keywords) = L1Summarizer::validate_and_build(&parsed, sid);
    assert!(l1.summary.contains("失败"));
}

#[test]
fn validate_time_period_invalid() {
    let parsed = L1SummaryResponse {
        summary: Some("测试摘要".into()),
        keywords: None,
        time_period: Some("午夜".into()), // 非法值
        atmosphere: None,
        valence: Some(0.0),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
    assert!(l1.time_period.is_none(), "非法 time_period 应被过滤");
}

#[test]
fn validate_atmosphere_truncation() {
    let parsed = L1SummaryResponse {
        summary: Some("测试摘要".into()),
        keywords: None,
        time_period: Some("上午".into()),
        atmosphere: Some("非常轻松愉快的一天".into()), // 9字
        valence: Some(0.5),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
    let atm = l1.atmosphere.unwrap();
    assert!(atm.chars().count() <= 4, "atmosphere 应截断到 ≤4 字: {atm}");
}

#[test]
fn validate_keywords_parsing() {
    let parsed = L1SummaryResponse {
        summary: Some("测试".into()),
        keywords: Some("工作, 学习, 编程".into()),
        time_period: Some("下午".into()),
        atmosphere: Some("专注高效".into()),
        valence: Some(0.0),
        salience: Some(0.5),
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    };
    let sid = ramaria_core::types::new_id();
    let (_l1, keywords) = L1Summarizer::validate_and_build(&parsed, sid);
    assert_eq!(keywords.len(), 3);
    assert!(keywords.contains(&KeywordToken::new("工作").unwrap()));
    assert!(keywords.contains(&KeywordToken::new("学习").unwrap()));
    assert!(keywords.contains(&KeywordToken::new("编程").unwrap()));
}

// ---- parse_summary_json (via pure helpers) ----

#[test]
fn parse_valid_json_direct() {
    let raw = r#"{"summary": "测试摘要", "valence": 0.5, "salience": 0.5}"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed.summary.unwrap(), "测试摘要");
}

#[test]
fn parse_with_think_tags() {
    let raw = "<think>reasoning</think>\n{\"summary\": \"测试\"}";
    let stripped = crate::utils::strip_thinking(raw);
    let parsed: L1SummaryResponse = serde_json::from_str(&stripped).unwrap();
    assert_eq!(parsed.summary.unwrap(), "测试");
}

#[test]
fn parse_with_prefix_text() {
    let raw = "这是前缀说明文字 {\"summary\": \"测试\", \"valence\": 0.0}";
    let extracted = crate::utils::extract_first_json_object(raw).unwrap();
    let parsed: L1SummaryResponse = serde_json::from_str(&extracted).unwrap();
    assert_eq!(parsed.summary.unwrap(), "测试");
}

// ---- 完整流程（需要 mock） ----
// 完整集成测试在 l1/mod.rs 的测试中，使用 mock LlmProvider + mock StorageBackend

// ---- situation_strength 解析 ----

#[test]
fn parse_situation_strength_from_json() {
    let raw = r#"{"summary": "测试", "valence": 0.0, "salience": 0.5, "situation_strength": 2}"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed.situation_strength, Some(2));
}

#[test]
fn parse_situation_strength_missing_defaults_none() {
    let raw = r#"{"summary": "测试", "valence": 0.0, "salience": 0.5}"#;
    let parsed: L1SummaryResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed.situation_strength, None);
}

#[test]
fn validate_and_build_does_not_inject_situation_strength() {
    // validate_and_build 只负责字段校验，situation_strength 的注入
    // 由调用方 generate_chunk_l1 完成（LLM 输出 > config > 默认 3）。
    // 此处验证注入不在此层发生：无论 LLM 是否输出该字段，
    // validate_and_build 产出的 L1 均为 None。
    for llm_value in [Some(5), None] {
        let parsed = L1SummaryResponse {
            summary: Some("测试摘要".into()),
            keywords: None,
            time_period: Some("上午".into()),
            atmosphere: Some("轻松".into()),
            valence: Some(0.5),
            salience: Some(0.5),
            situation_strength: llm_value,
            evidence_notes: None,
            continuation: None,
        };
        let sid = ramaria_core::types::new_id();
        let (l1, _) = L1Summarizer::validate_and_build(&parsed, sid);
        assert_eq!(
            l1.situation_strength, None,
            "validate_and_build 不应注入 situation_strength（LLM 输入 {llm_value:?}）"
        );
    }
}

/// 真实注入路径（generate_chunk_l1 步骤 7）：
/// LLM 输出 > config 回退 > 默认 3。
#[tokio::test]
async fn summarize_session_injects_situation_strength_priority() {
    use crate::l1::mock::MockLlmProvider;

    // 场景 A：LLM 输出 situation_strength=5 → 优先采用
    let sid_a = Uuid::new_v4();
    let storage_a = MockStorage::new();
    storage_a.add_messages(
        sid_a,
        vec![
            make_msg(sid_a, MessageRole::User, "最近压力好大"),
            make_msg(sid_a, MessageRole::Assistant, "辛苦了，早点休息"),
        ],
    );
    let llm_a = MockLlmProvider::new("test-model");
    llm_a.set_response(
        serde_json::json!({
            "summary": "测试摘要",
            "keywords": "压力",
            "time_period": "上午",
            "atmosphere": "平静",
            "valence": -0.4,
            "salience": 0.5,
            "situation_strength": 5,
            "evidence_notes": []
        })
        .to_string(),
    );
    let summarizer_a = L1Summarizer::new(
        &llm_a,
        &storage_a,
        L1SummarizerConfig {
            utt_splitter: None,
            ..Default::default()
        },
    );
    summarizer_a
        .summarize_session(sid_a)
        .await
        .expect("场景 A 应成功");
    assert_eq!(
        storage_a.saved_l1_entries()[0].situation_strength,
        Some(5),
        "LLM 输出优先于 config 与默认值"
    );

    // 场景 B：LLM 缺失 + config=Some(2) → 回退 config
    let sid_b = Uuid::new_v4();
    let storage_b = MockStorage::new();
    storage_b.add_messages(
        sid_b,
        vec![
            make_msg(sid_b, MessageRole::User, "最近压力好大"),
            make_msg(sid_b, MessageRole::Assistant, "辛苦了，早点休息"),
        ],
    );
    let llm_b = MockLlmProvider::new("test-model");
    llm_b.set_response(llm_json("测试摘要", None)); // 无 situation_strength
    let summarizer_b = L1Summarizer::new(
        &llm_b,
        &storage_b,
        L1SummarizerConfig {
            situation_strength: Some(2),
            utt_splitter: None,
            ..Default::default()
        },
    );
    summarizer_b
        .summarize_session(sid_b)
        .await
        .expect("场景 B 应成功");
    assert_eq!(
        storage_b.saved_l1_entries()[0].situation_strength,
        Some(2),
        "LLM 缺失时应回退 config 值"
    );

    // 场景 C：LLM 缺失 + config=None → 默认 3
    let sid_c = Uuid::new_v4();
    let storage_c = MockStorage::new();
    storage_c.add_messages(
        sid_c,
        vec![
            make_msg(sid_c, MessageRole::User, "最近压力好大"),
            make_msg(sid_c, MessageRole::Assistant, "辛苦了，早点休息"),
        ],
    );
    let llm_c = MockLlmProvider::new("test-model");
    llm_c.set_response(llm_json("测试摘要", None));
    let summarizer_c = L1Summarizer::new(
        &llm_c,
        &storage_c,
        L1SummarizerConfig {
            utt_splitter: None,
            ..Default::default()
        },
    );
    summarizer_c
        .summarize_session(sid_c)
        .await
        .expect("场景 C 应成功");
    assert_eq!(
        storage_c.saved_l1_entries()[0].situation_strength,
        Some(3),
        "LLM 与 config 均缺失时回退默认 3"
    );
}

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

// =========================================================
// summarize_session 集成测试
// =========================================================

/// 测试 summarize_session 完整流程：消息→格式化→mock LLM→解析→校验→存储。
#[tokio::test]
async fn summarize_session_integration_basic() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();

    // 准备 mock 存储：3 条对话消息
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![
            make_msg(session_id, MessageRole::User, "今天天气真不错"),
            make_msg(session_id, MessageRole::Assistant, "是啊，适合出去走走"),
            make_msg(session_id, MessageRole::User, "不过最近工作有点累"),
        ],
    );
    storage.set_keywords(vec!["天气".into(), "工作".into(), "疲惫".into()]);

    // 准备 mock LLM：返回有效 JSON（使用 serde_json 构造确保格式正确）
    let llm = MockLlmProvider::new("test-model");
    let response_json = serde_json::json!({
        "summary": "用户和助手聊了天气和最近的工作状态",
        "keywords": "天气,工作压力,日常闲聊",
        "time_period": "上午",
        "atmosphere": "轻松闲聊",
        "valence": 0.3,
        "salience": 0.5,
        "evidence_notes": ["用户说天气不错", "用户提到最近工作有点累"]
    });
    llm.set_response(response_json.to_string());

    let config = L1SummarizerConfig {
        persona_uid: Some("test-persona".into()),
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);

    let result = summarizer.summarize_session(session_id).await;
    assert!(
        result.is_ok(),
        "summarize_session 应成功: {:?}",
        result.err()
    );

    let l1 = result.unwrap();
    assert_eq!(l1.persona_uid, Some("test-persona".into()));
    assert!(l1.summary.contains("天气"), "摘要应包含天气相关内容");
    assert!(
        !l1.evidence_notes.as_ref().unwrap().is_empty(),
        "evidence_notes 不应为空"
    );

    // 验证存储写入
    let saved = storage.saved_l1_entries();
    assert_eq!(saved.len(), 1, "应保存 1 条 L1 记录");
    assert!(storage.keyword_count() >= 1, "应写入至少 1 个关键词");
}

/// 测试空消息 session 返回错误。
#[tokio::test]
async fn summarize_session_empty_messages_errors() {
    use crate::l1::mock::{MockLlmProvider, MockStorage};
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    let llm = MockLlmProvider::new("test-model");

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(session_id).await;
    assert!(result.is_err(), "空消息 session 应返回错误");
}

/// 测试 LLM 返回 JSON 中 evidence_notes 缺失时降级。
#[tokio::test]
async fn summarize_session_missing_evidence_notes_degrades() {
    use crate::l1::mock::{MockLlmProvider, MockStorage, make_msg};
    use ramaria_core::types::MessageRole;
    use uuid::Uuid;

    let session_id = Uuid::new_v4();
    let storage = MockStorage::new();
    storage.add_messages(
        session_id,
        vec![make_msg(session_id, MessageRole::User, "测试消息")],
    );

    let llm = MockLlmProvider::new("test-model");
    // 不包含 evidence_notes 字段
    let response_json = serde_json::json!({
        "summary": "一条测试消息",
        "keywords": "测试",
        "time_period": "未知",
        "atmosphere": "中性",
        "valence": 0.0,
        "salience": 0.3
    });
    llm.set_response(response_json.to_string());

    let config = L1SummarizerConfig {
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        temperature: 0.3,
        max_tokens: 2048,
        user_prefix: "用户：".into(),
        assistant_prefix: "助手：".into(),
        utt_splitter: None,
        prior_context_threshold: 20,
        prior_context_max_chars: 1500,
    };

    let summarizer = L1Summarizer::new(&llm, &storage, config);
    let result = summarizer.summarize_session(session_id).await;
    assert!(
        result.is_ok(),
        "缺少 evidence_notes 不应阻塞流程: {:?}",
        result.err()
    );

    let l1 = result.unwrap();
    // evidence_notes 缺失时降级为空数组
    let notes = l1.evidence_notes.expect("evidence_notes 应为 Some");
    assert!(notes.is_empty(), "缺失 evidence_notes 时应降级为空数组");
}

// =========================================================
// B2 上下文感知生成测试
// =========================================================

use crate::utt::{UttChunk, UttSplitterConfig};

/// 构造带 persona_uid 的 assistant 消息（目标发言）。
fn target_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
    let mut m = make_msg(session_id, MessageRole::Assistant, content);
    m.created_at = created_at;
    m.persona_uid = Some("char-0001".to_string());
    m
}

/// 构造用户消息（非目标侧）。
fn user_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
    let mut m = make_msg(session_id, MessageRole::User, content);
    m.created_at = created_at;
    m
}

/// 构造一个消息块。
fn make_chunk(msgs: Vec<Message>) -> UttChunk {
    UttChunk::from_messages(msgs)
}

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

/// 构造带 continuation 的 mock LLM 响应 JSON。
fn llm_json(summary: &str, continuation: Option<&str>) -> String {
    let mut obj = serde_json::json!({
        "summary": summary,
        "keywords": "测试,关键词",
        "time_period": "上午",
        "atmosphere": "平静",
        "valence": 0.0,
        "salience": 0.5,
        "evidence_notes": []
    });
    if let Some(c) = continuation {
        obj["continuation"] = serde_json::json!(c);
    }
    obj.to_string()
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
        "最后一段应覆盖最新对话（封存只摘要尾部）"
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
