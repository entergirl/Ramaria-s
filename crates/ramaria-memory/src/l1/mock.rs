//! crates/ramaria-memory/src/l1/mock.rs - L1 管线测试用 mock 与数据校验
//!
//! 设计特点:
//! - MockLlmProvider / MockStorage 单份实现下沉到 ramaria-testkit，本模块只做转出
//! - make_msg / make_valid_l1 提供确定性 L1 与消息夹具
//! - validate_l1_entry 校验 L1 字段完整性，失败即 panic（测试数据错误）
//! - 仅 #[cfg(test)] 编译，零运行时开销

#![allow(dead_code)]

use uuid::Uuid;

pub use ramaria_testkit::mock::{MockLlmProvider, MockStorage};

// =========================================================
// 辅助函数：构造测试用消息
// =========================================================

/// 创建测试用消息。
pub fn make_msg(
    session_id: Uuid,
    role: ramaria_core::types::MessageRole,
    content: &str,
) -> ramaria_core::types::Message {
    ramaria_core::types::Message::new(
        session_id,
        role,
        content.to_string(),
        ramaria_core::types::MessageSource::Local,
    )
}

/// 校验 L1 记忆条目的字段完整性。
///
/// 校验项:
/// - summary: 不能为空
/// - time_period: 若存在则必须是 清晨/上午/下午/傍晚/夜间/深夜 之一
/// - atmosphere: 若存在则最多 4 个字符
/// - valence: 范围 [-1.0, 1.0]
/// - salience: 范围 [0.0, 1.0]
///
/// 验证失败时 panic，因为测试数据中的无效值表示测试本身的 bug。
pub fn validate_l1_entry(l1: &ramaria_core::types::MemoryL1) {
    let mut errors: Vec<String> = Vec::new();

    if l1.summary.trim().is_empty() {
        errors.push("summary 为空".to_string());
    }

    if let Some(ref tp) = l1.time_period {
        const VALID_PERIODS: &[&str] = &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"];
        if !VALID_PERIODS.contains(&tp.as_str()) {
            errors.push(format!(
                "time_period 值非法: '{}'，合法值: {:?}",
                tp, VALID_PERIODS
            ));
        }
    }

    if let Some(ref atm) = l1.atmosphere {
        let len = atm.chars().count();
        if len > 4 {
            errors.push(format!(
                "atmosphere 过长 ({} 字): '{}'，应 ≤ 4 字",
                len, atm
            ));
        }
    }

    if l1.valence < -1.0 || l1.valence > 1.0 {
        errors.push(format!(
            "valence 超出范围 [{:.2}]，应为 [-1.0, 1.0]",
            l1.valence
        ));
    }

    if l1.salience < 0.0 || l1.salience > 1.0 {
        errors.push(format!(
            "salience 超出范围 [{:.2}]，应为 [0.0, 1.0]",
            l1.salience
        ));
    }

    if !errors.is_empty() {
        panic!(
            "L1 条目字段校验失败:\n  l1_id={}\n  session_id={}\n  summary='{}'\n  错误列表:\n    {}",
            l1.id,
            l1.session_id,
            l1.summary,
            errors.join("\n    ")
        );
    }
}

/// 创建一条完全合法的测试用 L1 条目。
pub fn make_valid_l1(summary: &str) -> ramaria_core::types::MemoryL1 {
    ramaria_core::types::MemoryL1 {
        id: ramaria_core::types::new_id(),
        session_id: ramaria_core::types::new_id(),
        summary: summary.to_string(),
        keywords: Some("测试, 关键字".to_string()),
        time_period: Some("上午".to_string()),
        atmosphere: Some("平静".to_string()),
        valence: 0.5,
        salience: 0.5,
        absorbed: false,
        created_at: ramaria_core::types::now_ms(),
        occurred_at: None,
        last_accessed_at: None,
        persona_uid: None,
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    }
}

#[cfg(test)]
mod mock_validation_tests {
    use super::*;

    #[test]
    fn validate_valid_l1_passes() {
        let l1 = make_valid_l1("测试摘要");
        validate_l1_entry(&l1);
    }

    #[test]
    #[should_panic(expected = "summary 为空")]
    fn validate_empty_summary_panics() {
        let l1 = make_valid_l1("");
        validate_l1_entry(&l1);
    }

    #[test]
    #[should_panic(expected = "time_period 值非法")]
    fn validate_invalid_time_period_panics() {
        let mut l1 = make_valid_l1("测试");
        l1.time_period = Some("午夜".to_string());
        validate_l1_entry(&l1);
    }

    #[test]
    #[should_panic(expected = "atmosphere 过长")]
    fn validate_long_atmosphere_panics() {
        let mut l1 = make_valid_l1("测试");
        l1.atmosphere = Some("这太长了超过四字".to_string());
        validate_l1_entry(&l1);
    }

    #[test]
    #[should_panic(expected = "valence 超出范围")]
    fn validate_out_of_range_valence_panics() {
        let mut l1 = make_valid_l1("测试");
        l1.valence = 2.0;
        validate_l1_entry(&l1);
    }

    #[test]
    #[should_panic(expected = "salience 超出范围")]
    fn validate_out_of_range_salience_panics() {
        let mut l1 = make_valid_l1("测试");
        l1.salience = -0.5;
        validate_l1_entry(&l1);
    }

    #[test]
    fn validate_none_time_period_passes() {
        let mut l1 = make_valid_l1("测试");
        l1.time_period = None;
        validate_l1_entry(&l1);
    }

    #[test]
    fn make_valid_l1_has_correct_fields() {
        let l1 = make_valid_l1("示例摘要");
        assert_eq!(l1.summary, "示例摘要");
        assert_eq!(l1.time_period.as_deref(), Some("上午"));
        assert!(!l1.absorbed);
        assert!((l1.valence - 0.5).abs() < f64::EPSILON);
        assert!((l1.salience - 0.5).abs() < f64::EPSILON);
    }
}
