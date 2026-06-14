//! rust/crates/ramaria-memory/src/event/degrade.rs - 事件提取降级策略
//!
//! 设计特点:
//! - 降级触发条件: LLM 返回的 JSON 无法解析为合法事件数组
//! - 降级策略: 将多条未吸收 L1 糅合成一条 confidence=0.5 的混合事件
//! - 降级事件保留所有 L1 的摘要和关键词，不丢失信息
//! - 降级事件标记 presentation=Mixed, salience=avg(L1_salience)
//! - 纯函数，不依赖 LLM 或存储

use ramaria_core::types::{MemoryEvent, MemoryL1, Presentation, now_ms};

// =========================================================
// 降级事件构建
// =========================================================

/// 降级事件配置。
#[derive(Debug, Clone)]
pub struct DegradeConfig {
    /// 降级事件的默认事实确凿度
    pub default_confidence: f64,
    /// 降级事件的默认分享意愿
    pub default_share: f64,
    /// 摘要最大拼接字符数
    pub max_summary_chars: usize,
}

impl Default for DegradeConfig {
    fn default() -> Self {
        Self {
            default_confidence: 0.5,
            default_share: 0.5,
            max_summary_chars: 500,
        }
    }
}

// =========================================================
// 降级事件构建函数
// =========================================================

/// 将多条未吸收 L1 降级为一条混合事件。
///
/// 触发条件:
/// - LLM JSON 解析全部失败
/// - LLM 返回空事件数组
///
/// 降级事件属性:
/// - title: "时间段混合事件"
/// - summary: 拼接所有 L1 摘要（截断到 max_summary_chars）
/// - confidence: 0.5（默认）
/// - salience: L1 salience 平均值
/// - valence: L1 valence 平均值
/// - presentation: Mixed
///
/// 参数:
/// - `persona_uid`: 事件归属的人格标识。
/// - `l1_list`: 未吸收的 L1 摘要列表。
/// - `config`: 降级配置。
///
/// 返回:
/// - 降级事件。即使 l1_list 为空也返回一条事件（标记为"无具体内容"）。
pub fn build_degraded_event(
    persona_uid: &str,
    l1_list: &[MemoryL1],
    config: &DegradeConfig,
) -> MemoryEvent {
    let now = now_ms();

    // 计算事件时间范围（单次遍历）
    let (start, end) = if l1_list.is_empty() {
        (now, now)
    } else {
        let mut min_ts = l1_list[0].created_at;
        let mut max_ts = l1_list[0].created_at;
        for l1 in &l1_list[1..] {
            if l1.created_at < min_ts {
                min_ts = l1.created_at;
            }
            if l1.created_at > max_ts {
                max_ts = l1.created_at;
            }
        }
        (min_ts, max_ts)
    };

    // 拼接摘要
    let summary = if l1_list.is_empty() {
        "（降级事件：无具体内容）".to_string()
    } else {
        build_degraded_summary(l1_list, config.max_summary_chars)
    };

    // 拼接关键词
    let keywords = build_degraded_keywords(l1_list);

    // 聚合 salience（平均值）
    let salience = if l1_list.is_empty() {
        0.5
    } else {
        let sum: f64 = l1_list.iter().map(|l| l.salience).sum();
        let avg = sum / l1_list.len() as f64;
        crate::utils::clamp_salience(avg)
    };

    // 聚合 valence（平均值）
    let valence = if l1_list.is_empty() {
        0.0
    } else {
        let sum: f64 = l1_list.iter().map(|l| l.valence).sum();
        let avg = sum / l1_list.len() as f64;
        crate::utils::clamp_valence(avg)
    };

    // 参与人数 = 来源 L1 数量
    let absorbed = l1_list.len() as i64;

    let title = if l1_list.is_empty() {
        "空事件（降级）".to_string()
    } else {
        format!("时间段混合事件（{}条摘要）", l1_list.len())
    };

    MemoryEvent {
        id: 0, // 由存储层回填
        persona_uid: persona_uid.to_string(),
        title,
        summary,
        keywords,
        participants: None,
        start,
        end,
        confidence: config.default_confidence,
        salience,
        valence,
        presentation: Presentation::Mixed,
        share: config.default_share,
        attitude: None,
        paraphrase: None,
        absorbed,
        situation_strength: None, // 降级事件无 L1 情境信息，等效 3
        created_at: now,
        last_accessed_at: None,
        indexed_at: None,
        index_version: None,
    }
}

// =========================================================
// 内部辅助函数
// =========================================================

/// 拼接 L1 摘要为降级事件 summary。
///
/// 格式: 每条 L1 摘要一行，前缀 `- `。
fn build_degraded_summary(l1_list: &[MemoryL1], max_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(l1_list.len());
    let mut total = 0usize;

    for l1 in l1_list {
        let line = format!("- {}", l1.summary);
        let line_len = line.chars().count();
        if total + line_len > max_chars && !parts.is_empty() {
            parts.push("...（后续摘要已截断）".to_string());
            break;
        }
        parts.push(line);
        total += line_len;
    }

    parts.join("\n")
}

/// 拼接 L1 关键词为降级事件 keywords。
///
/// 格式: 去重后的逗号分隔列表。
fn build_degraded_keywords(l1_list: &[MemoryL1]) -> Option<String> {
    let mut all_kw: Vec<String> = Vec::new();
    for l1 in l1_list {
        if let Some(ref kws) = l1.keywords {
            for kw in kws.split(',') {
                let kw = kw.trim().to_string();
                if !kw.is_empty() && !all_kw.contains(&kw) {
                    all_kw.push(kw);
                }
            }
        }
    }

    if all_kw.is_empty() {
        None
    } else {
        Some(all_kw.join(", "))
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::now_ms;
    use ramaria_core::types::{MemoryL1, Presentation};
    use uuid::Uuid;

    fn make_l1(
        session_id: Uuid,
        summary: &str,
        keywords: Option<&str>,
        valence: f64,
        salience: f64,
    ) -> MemoryL1 {
        MemoryL1 {
            id: Uuid::new_v4(),
            session_id,
            summary: summary.to_string(),
            keywords: keywords.map(|s| s.to_string()),
            time_period: Some("下午".to_string()),
            atmosphere: None,
            valence,
            salience,
            absorbed: false,
            created_at: now_ms(),
            last_accessed_at: None,
            persona_uid: None,
            context_json: None,
            situation_strength: None,
        }
    }

    #[test]
    fn degraded_event_empty_l1_list() {
        let event = build_degraded_event("user-0001", &[], &DegradeConfig::default());
        assert_eq!(event.persona_uid, "user-0001");
        assert_eq!(event.confidence, 0.5);
        assert_eq!(event.salience, 0.5);
        assert_eq!(event.valence, 0.0);
        assert_eq!(event.presentation, Presentation::Mixed);
        assert!(event.title.contains("降级"));
    }

    #[test]
    fn degraded_event_single_l1() {
        let sid = Uuid::new_v4();
        let l1_list = vec![make_l1(sid, "用户去了医院", Some("医院, 健康"), -0.5, 0.75)];
        let event = build_degraded_event("user-0001", &l1_list, &DegradeConfig::default());

        assert!(event.summary.contains("用户去了医院"));
        assert_eq!(event.absorbed, 1);
        assert!(event.salience > 0.5); // 平均 0.75 钳制到 0.75
        assert!(event.valence < 0.0); // -0.5 钳制到 -0.5
        assert!(event.keywords.is_some());
        assert!(event.keywords.unwrap().contains("医院"));
    }

    #[test]
    fn degraded_event_multiple_l1() {
        let sid1 = Uuid::new_v4();
        let sid2 = Uuid::new_v4();
        let l1_list = vec![
            make_l1(sid1, "用户去看了电影", Some("电影, 娱乐"), 0.5, 0.5),
            make_l1(sid2, "用户完成了工作报告", Some("工作, 报告"), 0.0, 0.25),
            make_l1(sid1, "用户和朋友聚餐", Some("社交, 聚餐"), 0.5, 0.75),
        ];
        let event = build_degraded_event("user-0001", &l1_list, &DegradeConfig::default());

        assert_eq!(event.absorbed, 3);
        assert!(event.summary.contains("电影"));
        assert!(event.summary.contains("工作"));
        assert!(event.summary.contains("聚餐"));

        // 关键词去重合并
        let kws = event.keywords.unwrap();
        assert!(kws.contains("电影"));
        assert!(kws.contains("工作"));
        assert!(kws.contains("社交"));
    }

    #[test]
    fn degraded_summary_truncation() {
        let sid = Uuid::new_v4();
        let mut l1_list = Vec::new();
        for i in 0..20 {
            l1_list.push(make_l1(
                sid,
                &format!("第{i}条摘要: 这是一段比较长的测试文本用于验证截断功能"),
                None,
                0.0,
                0.5,
            ));
        }

        let config = DegradeConfig {
            max_summary_chars: 200,
            ..Default::default()
        };
        let event = build_degraded_event("user-0001", &l1_list, &config);
        assert!(event.summary.chars().count() <= 250); // 允许少量超出
        assert!(event.summary.contains("已截断"));
    }

    #[test]
    fn clamp_salience_values() {
        assert!((crate::utils::clamp_salience(0.3) - 0.25).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.9) - 1.0).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_salience(0.0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_valence_values() {
        assert!((crate::utils::clamp_valence(-0.7) - (-0.5)).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(0.3) - 0.5).abs() < f64::EPSILON);
        assert!((crate::utils::clamp_valence(-1.0) - (-1.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn degraded_event_no_keywords() {
        let sid = Uuid::new_v4();
        let l1_list = vec![make_l1(sid, "无关键词摘要", None, 0.0, 0.5)];
        let event = build_degraded_event("user-0001", &l1_list, &DegradeConfig::default());
        assert!(event.keywords.is_none());
    }

    #[test]
    fn degraded_event_start_end_times() {
        let now = now_ms();
        let sid = Uuid::new_v4();
        let mut l1_old = make_l1(sid, "旧摘要", None, 0.0, 0.5);
        l1_old.created_at = now - 86_400_000; // 1 天前

        let mut l1_new = make_l1(sid, "新摘要", None, 0.0, 0.5);
        l1_new.created_at = now;

        let event = build_degraded_event("user-0001", &[l1_old, l1_new], &DegradeConfig::default());
        assert_eq!(event.start, now - 86_400_000);
        assert_eq!(event.end, now);
    }
}
