//! crates/ramaria-memory/src/event/degrade.rs - 事件提取降级策略
//!
//! 设计特点:
//! - 降级触发条件: LLM 返回的 JSON 无法解析为合法事件数组
//! - 降级策略: 将多条未吸收 L1 糅合成一条混合事件
//! - 降级置信度: `min(0.59, 0.35 + 0.02 × n_l1)`，封顶 0.59 恒处 tentative
//! - 合并的 L1 越多越说明"这是一段有内容的对话"，置信度越高；n_l1=12 达上界 0.59
//! - 降级事件保留所有 L1 的摘要和关键词，不丢失信息
//! - 降级事件标记 presentation=Mixed, salience=avg(L1_salience)
//! - `dynamic_confidence_enabled=false` 时回退固定 `default_confidence`（0.5）
//! - 纯函数，不依赖 LLM 或存储

use ramaria_core::types::{MemoryEvent, MemoryL1, Presentation, now_ms};

// =========================================================
// 降级事件构建
// =========================================================

/// 降级事件配置。
#[derive(Debug, Clone)]
pub struct DegradeConfig {
    /// 降级事件的默认事实确凿度（`dynamic_confidence_enabled=false` 时使用）
    pub default_confidence: f64,
    /// 降级事件的默认分享意愿
    pub default_share: f64,
    /// 摘要最大拼接字符数
    pub max_summary_chars: usize,
    /// 是否启用动态置信度公式。
    /// `true` → `min(0.59, 0.35 + 0.02 × n_l1)`；`false` → 固定 `default_confidence`。
    pub dynamic_confidence_enabled: bool,
    /// 动态置信度公式封顶值（默认 0.59，恒处 tentative 区间 < 0.6）。
    pub dynamic_confidence_cap: f64,
    /// 动态置信度公式基础偏移量（默认 0.35）。
    pub dynamic_confidence_base: f64,
    /// 动态置信度公式每 L1 增量系数（默认 0.02）。
    pub dynamic_confidence_per_l1: f64,
}

impl Default for DegradeConfig {
    fn default() -> Self {
        Self {
            default_confidence: 0.5,
            default_share: 0.5,
            max_summary_chars: 500,
            dynamic_confidence_enabled: true,
            dynamic_confidence_cap: 0.59,
            dynamic_confidence_base: 0.35,
            dynamic_confidence_per_l1: 0.02,
        }
    }
}

/// 计算降级事件动态置信度。
///
/// 公式:
/// `conf = min(cap, base + per_l1 × n_l1)`，取值区间 [0.0, cap]。
///
/// 语义:
/// - 合并了越多 L1 的降级事件越说明"这是一段有内容的对话"，置信度越高；
/// - 封顶 `cap`（默认 0.59）确保其恒处于 tentative 区间（始终低于 confirmed 门槛 0.6）；
/// - `n_l1=12` 时 `conf=0.59`（tentative 上界）；`n_l1=2` 时 `conf=0.39` 合理排除。
///
/// 参数:
/// - `n_l1`: 糅合进降级事件的未吸收 L1 数量。
/// - `config`: 降级配置（含 cap/base/per_l1）。
///
/// 返回:
/// - 动态置信度，钳制在 [0.0, cap]。
pub fn degraded_confidence(n_l1: usize, config: &DegradeConfig) -> f64 {
    let raw = config.dynamic_confidence_base + config.dynamic_confidence_per_l1 * n_l1 as f64;
    raw.clamp(0.0, config.dynamic_confidence_cap)
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
/// - confidence: 动态公式 `min(0.59, 0.35 + 0.02 × n_l1)`（封顶 0.59 恒 tentative），
///   关闭动态公式时回退固定 `default_confidence`（0.5）
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

    // 动态置信度：合并 L1 越多越有内容，封顶 0.59 恒处 tentative。
    // 关闭动态公式时回退固定 default_confidence。
    let confidence = if config.dynamic_confidence_enabled {
        degraded_confidence(l1_list.len(), config)
    } else {
        config.default_confidence
    };

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
        salience,
        valence,
        presentation: Presentation::Mixed,
        share: config.default_share,
        confidence,
        attitude: None,
        paraphrase: None,
        absorbed,
        situation_strength: None, // 降级事件无 L1 情境信息，等效 3
        motives: None,
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
///
/// 使用 `HashSet` 进行 O(1) 去重，替代原来的 `Vec::contains` O(n²) 实现。
/// 降级路径仅在 LLM JSON 解析失败时调用，每次最多约 20 条 L1 记录，
/// 性能影响极小，但 HashSet 语义更清晰。
fn build_degraded_keywords(l1_list: &[MemoryL1]) -> Option<String> {
    use std::collections::HashSet;
    let mut all_kw: HashSet<String> = HashSet::new();
    for l1 in l1_list {
        if let Some(ref kws) = l1.keywords {
            for kw in kws.split(',') {
                let kw = kw.trim();
                if !kw.is_empty() {
                    all_kw.insert(kw.to_string());
                }
            }
        }
    }

    if all_kw.is_empty() {
        None
    } else {
        // 转为 Vec 后排序以保证输出稳定（便于测试）
        let mut sorted: Vec<&String> = all_kw.iter().collect();
        sorted.sort();
        Some(
            sorted
                .into_iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
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
            evidence_notes: None,
            continuation: None,
        }
    }

    #[test]
    fn degraded_event_empty_l1_list() {
        let event = build_degraded_event("user-0001", &[], &DegradeConfig::default());
        assert_eq!(event.persona_uid, "user-0001");
        // 动态公式: 空列表 → min(0.59, 0.35 + 0.02×0) = 0.35
        assert!((event.confidence - 0.35).abs() < 1e-9);
        assert_eq!(event.salience, 0.5);
        assert_eq!(event.valence, 0.0);
        assert_eq!(event.presentation, Presentation::Mixed);
        assert!(event.title.contains("降级"));
    }

    /// 动态置信度公式边界。
    ///
    /// - n_l1 小（2）→ 0.39（合理排除）；
    /// - n_l1 达到封顶（12）→ 0.59（tentative 上界）；
    /// - n_l1 超大 → 仍封顶 0.59（不越过 confirmed 门槛 0.6）。
    #[test]
    fn degraded_confidence_formula_boundaries() {
        let cfg = DegradeConfig::default();

        // n_l1 = 0 → 0.35
        assert!((degraded_confidence(0, &cfg) - 0.35).abs() < 1e-9);
        // n_l1 = 2 → 0.39
        assert!((degraded_confidence(2, &cfg) - 0.39).abs() < 1e-9);
        // n_l1 = 12 → 0.59（封顶）
        assert!((degraded_confidence(12, &cfg) - 0.59).abs() < 1e-9);
        // n_l1 = 100 → 仍封顶 0.59
        assert!((degraded_confidence(100, &cfg) - 0.59).abs() < 1e-9);
        // 恒处 tentative（< 0.6）
        for n in 0..100 {
            assert!(degraded_confidence(n, &cfg) < 0.6, "n={n} 应恒 < 0.6");
        }
    }

    /// 关闭动态公式时回退固定 0.5。
    ///
    /// 说明: `degraded_confidence` 是纯公式函数（始终计算动态公式），
    /// 配置开关 `dynamic_confidence_enabled=false` 由 `build_degraded_event` 层裁决——
    /// 关闭时回退固定 `default_confidence`（0.5）。
    #[test]
    fn degraded_confidence_fallback() {
        let cfg = DegradeConfig {
            dynamic_confidence_enabled: false,
            ..Default::default()
        };
        // 无论 n_l1 多少，关闭动态公式 → build_degraded_event 固定 default_confidence = 0.5
        let e_empty = build_degraded_event("user-0001", &[], &cfg);
        assert!(
            (e_empty.confidence - 0.5).abs() < 1e-9,
            "conf={}",
            e_empty.confidence
        );

        let sid = Uuid::new_v4();
        let twelve: Vec<MemoryL1> = (0..12)
            .map(|i| make_l1(sid, &format!("L{i}"), None, 0.0, 0.5))
            .collect();
        let e12 = build_degraded_event("user-0001", &twelve, &cfg);
        assert!(
            (e12.confidence - 0.5).abs() < 1e-9,
            "conf={}",
            e12.confidence
        );
    }

    /// 动态公式在 build_degraded_event 中生效（按 L1 数量变化）。
    #[test]
    fn degraded_confidence_applied_in_event() {
        let cfg = DegradeConfig::default();
        let sid = Uuid::new_v4();

        // 2 条 L1 → 0.39
        let two = vec![
            make_l1(sid, "A", None, 0.0, 0.5),
            make_l1(sid, "B", None, 0.0, 0.5),
        ];
        let e2 = build_degraded_event("user-0001", &two, &cfg);
        assert!(
            (e2.confidence - 0.39).abs() < 1e-9,
            "conf={}",
            e2.confidence
        );

        // 12 条 L1 → 0.59（封顶）
        let twelve: Vec<MemoryL1> = (0..12)
            .map(|i| make_l1(sid, &format!("L{i}"), None, 0.0, 0.5))
            .collect();
        let e12 = build_degraded_event("user-0001", &twelve, &cfg);
        assert!(
            (e12.confidence - 0.59).abs() < 1e-9,
            "conf={}",
            e12.confidence
        );
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
