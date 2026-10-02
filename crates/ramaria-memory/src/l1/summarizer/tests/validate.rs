//! crates/ramaria-memory/src/l1/summarizer/tests/validate.rs - JSON 解析与字段校验
//!
//! 设计特点:
//! - 由 父测试模块 以 mod validate; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

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
