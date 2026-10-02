//! crates/ramaria-memory/src/inference/inferrer/mock.rs - 无 LLM 依赖的规则推断
//!
//! 设计特点:
//! - 基于 StatsSummary 生成确定性人格标签，支持无 LLM 测试与 CI 环境
//! - 话题特征词黑名单：避免将话题名（如"沉浸体验"）当作性格标签输出
//! - 动机维度驱动：从显著动机统计生成点缀 trait
//! - 置信度按证据量与一致性动态计算，不做统一硬编码
//! - 推断逻辑透明可审计，不调用真实 LLM

use ramaria_core::{PersonalityTrait, TraitLayer, TraitSource, TraitStatus};

use crate::inference::stats::{CategoryStats, StatsSummary};

use super::types::{CategorySignal, ConsistencyAnalysis, InferenceResult};

// =========================================================
// Mock 推断（无 LLM 依赖的测试支持）
// =========================================================

/// 基于统计摘要的简易规则推断（Mock LLM 替代）。
///
/// 策略:
/// - 不调用真实 LLM，直接从统计指标推演出 PersonalityTrait 记录。
/// - 用于测试和 CI 环境，确保全管线可验证。
/// - 推断逻辑透明可审计。
///
/// 参数:
/// - `stats`: 统计摘要。
/// - `persona_uid`: 目标人格标识。
///
/// 返回:
/// - 推断结果（含 Step1/2/3 的完整输出）。
pub fn mock_infer(stats: &StatsSummary, persona_uid: &str) -> InferenceResult {
    let mut category_signals = Vec::new();
    let mut trait_seq = 0i32;

    // Step 1: 逐分类生成信号
    for cat in &stats.categories {
        let sufficient = cat.n_eff >= 5.0;

        // 话题特征词黑名单。
        // L2 事件提取产出的 category 是话题聚类结果（如"沉浸体验""系统逻辑"），
        // 而非性格维度。若统计指标不足以提炼出性格信号，标记为"insufficient_data"
        // 而非直接用话题名生成伪性格标签。
        const TOPIC_BLACKLIST: &[&str] = &[
            "沉浸体验",
            "系统逻辑",
            "叙事驱动",
            "规则构建",
            "角色带入",
            "AI模拟",
            "卡面模拟",
            "游戏",
            "技术",
            "编程",
            "开发",
            "界面设计",
            "数值系统",
            "世界观设定",
        ];

        let is_topic_category = TOPIC_BLACKLIST.iter().any(|kw| cat.category.contains(kw));

        // 优先使用最显著的信号维度
        let (signal_label, stability) = if is_topic_category && !sufficient {
            // 话题类分类 + 低证据量 → 不足以推断性格信号
            (format!("{}-数据不足", cat.category), "uncertain")
        } else if cat.valence_mean > 0.4 && cat.valence_std < 0.4 {
            (format!("{}-积极稳定", cat.category), "stable")
        } else if cat.valence_mean < -0.3 {
            (format!("{}-消极回避", cat.category), "contextual")
        } else if cat.share_mean > 0.7 {
            (format!("{}-高分享", cat.category), "contextual")
        } else if cat.share_mean < 0.3 {
            (format!("{}-低分享", cat.category), "contextual")
        } else if cat.presentation_subjective_ratio > 0.6 {
            (format!("{}-主观表达", cat.category), "contextual")
        } else if cat.presentation_objective_ratio > 0.6 {
            (format!("{}-客观理性", cat.category), "contextual")
        } else if cat.valence_std > 0.6 {
            (format!("{}-情绪波动", cat.category), "contextual")
        } else {
            (format!("{}-中性投入", cat.category), "contextual")
        };

        category_signals.push(CategorySignal {
            category: cat.category.clone(),
            signal_label,
            evidence_citation: format!(
                "valence_mean={:.2}, share_mean={:.2}, n_eff={:.1}",
                cat.valence_mean, cat.share_mean, cat.n_eff
            ),
            stability_judgment: stability.to_string(),
            sufficient_evidence: sufficient,
        });
    }

    // Step 2: 跨分类分析
    let mut base_candidates = Vec::new();
    let mut primary_candidates = Vec::new();
    let mut accent_candidates = Vec::new();

    // 在 ≥2 个分类中出现且稳定性为 "stable" 的信号 → 底色候选
    let mut signal_freq: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for sig in &category_signals {
        *signal_freq.entry(sig.signal_label.clone()).or_default() += 1;
    }
    for (label, freq) in &signal_freq {
        if *freq >= 2 {
            base_candidates.push(label.clone());
        }
    }

    // 最高权重分类 → 主色调
    if let Some(top) = stats.categories.first()
        && let Some(sig) = category_signals.iter().find(|s| s.category == top.category)
    {
        primary_candidates.push(sig.signal_label.clone());
    }

    // 矛盾检测 → 点缀
    if stats.cross_category.attitude_contradiction_count > 0 {
        accent_candidates.push("内在矛盾型".to_string());
    }
    // n_eff 很小的分类 → 点缀
    for sig in &category_signals {
        if !sig.sufficient_evidence
            && sig.signal_label != "insufficient_data"
            && !accent_candidates.contains(&sig.signal_label)
        {
            accent_candidates.push(sig.signal_label.clone());
        }
    }

    // ---- 动机维度的点缀特征 ----
    // 从动机统计中提取显著的动机驱动模式作为点缀 trait
    for motive_stat in stats.motive_stats.iter().take(3) {
        // 仅对 n_eff >= 2.0 的动机生成信号
        if motive_stat.n_eff < 2.0 {
            continue;
        }
        // 根据动机的效价模式生成 signal label
        let motive_signal = if motive_stat.valence_mean > 0.3 && motive_stat.valence_std < 0.5 {
            format!("动机-{}-正向驱动", motive_stat.motive)
        } else if motive_stat.valence_mean < -0.3 {
            format!("动机-{}-负向驱动", motive_stat.motive)
        } else if motive_stat.share_mean > 0.6 {
            format!("动机-{}-高分享", motive_stat.motive)
        } else if motive_stat.presentation_subjective_ratio > 0.6 {
            format!("动机-{}-主观表达", motive_stat.motive)
        } else {
            format!("动机-{}-驱动", motive_stat.motive)
        };
        // 仅当动机信号不在已有候选里且不重复时才添加
        if !accent_candidates.contains(&motive_signal) && accent_candidates.len() < 8 {
            accent_candidates.push(motive_signal);
        }
    }

    let consistency = ConsistencyAnalysis {
        base_candidates,
        primary_candidates,
        accent_candidates,
        notes: "Mock 推断——基于统计阈值的规则推演。真实环境应替换为 LLM 推断。".to_string(),
    };

    // Step 3: 生成 PersonalityTrait
    let mut traits = Vec::new();
    let now = ramaria_core::types::now_ms();

    // 根据分类统计指标动态计算 evidence 和 consistency，
    // 避免所有 trait 使用相同的硬编码初始值（导致统一 47% 置信度）
    let compute_mock_evidence = |n_eff: f64| n_eff.clamp(0.0, 100.0);
    let compute_mock_consistency = |valence_std: f64, share_std: f64| {
        let avg_std = (valence_std + share_std) / 2.0;
        (1.0 - avg_std).clamp(0.1, 0.95)
    };
    let compute_mock_confidence = |evidence: f64, consistency: f64| {
        if evidence <= 0.0 {
            0.0
        } else {
            consistency * (1.0 - 1.0 / (1.0 + evidence))
        }
    };

    // 从信号标签中匹配对应分类的统计指标。
    // 信号标签格式为 "{category}-{signal}"（如"工作-积极稳定"），
    // 通过遍历所有 category 检查标签前缀来匹配。
    let find_stats_for_signal = |signal_label: &str| -> Option<(&CategoryStats, f64, f64)> {
        stats
            .categories
            .iter()
            .find(|c| signal_label.starts_with(&c.category))
            .map(|cs| {
                let ev = compute_mock_evidence(cs.n_eff);
                let con = compute_mock_consistency(cs.valence_std, cs.share_std);
                (cs, ev, con)
            })
    };

    // 底色
    for (i, label) in consistency.base_candidates.iter().enumerate().take(3) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev, con))
            .unwrap_or((1.0, 0.5));
        let confidence = compute_mock_confidence(evidence, consistency);

        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Base,
            trait_label: label.clone(),
            meaning: format!("在多个生活领域表现出'{}'模式", label),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence,
            evidence,
            consistency,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
        trait_seq = i as i32 + 1;
    }

    // 主色调
    for (i, label) in consistency.primary_candidates.iter().enumerate().take(2) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev, con))
            .unwrap_or((1.0, 0.5));
        let confidence = compute_mock_confidence(evidence, consistency);

        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Primary,
            trait_label: label.clone(),
            meaning: format!("最突出地表现为'{}'", label),
            not_meaning: None,
            trigger: None,
            suppress: None,
            related: None,
            seq: trait_seq + i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence,
            evidence,
            consistency,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
    }
    trait_seq += consistency.primary_candidates.len().min(2) as i32;

    // 点缀
    for (i, label) in consistency.accent_candidates.iter().enumerate().take(4) {
        let (evidence, consistency) = find_stats_for_signal(label)
            .map(|(_cs, ev, con)| (ev * 0.5, con * 0.7))
            // 点缀层证据量较低，总体折扣
            .unwrap_or((0.5, 0.3));
        // 动机维度标签（"动机-xxx-驱动"）或"内在矛盾型"取默认值
        let (evidence, consistency) = if label.starts_with("动机-") || label.starts_with("内在矛盾")
        {
            (0.5, 0.3)
        } else {
            (evidence, consistency)
        };
        let confidence = compute_mock_confidence(evidence, consistency);

        traits.push(PersonalityTrait {
            id: 0,
            persona_uid: persona_uid.to_string(),
            layer: TraitLayer::Accent,
            trait_label: label.clone(),
            meaning: format!("在特定条件下浮现'{}'特质", label),
            not_meaning: None,
            trigger: Some("特定领域或低样本量条件下".to_string()),
            suppress: None,
            related: None,
            seq: trait_seq + i as i32,
            source: TraitSource::Inferred,
            ref_event_id: None,
            ref_l1_id: None,
            confidence,
            evidence,
            consistency,
            status: TraitStatus::Active,
            created_at: now,
            updated_at: now,
        });
    }

    InferenceResult {
        category_signals,
        consistency,
        traits,
    }
}
