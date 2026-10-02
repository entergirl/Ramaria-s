//! crates/ramaria-memory/src/inference/causal/format.rs - A8 因果链特征文本格式化
//!
//! 设计特点:
//! - 将因果链特征渲染为 Phase B Prompt 可注入的结构化中文文本
//! - 扩展段（时延分布 / 情绪沿链走势）为空时整段跳过，保持旧格式逐字节等价
//! - 无任何特征时返回空字符串，调用方可直接跳过注入
//! - 纯函数，无 I/O 与副作用

use super::extract::MS_PER_DAY;
use super::types::CausalChainFeatures;

// =========================================================
// 文本格式化（供 Prompt 注入）
// =========================================================

/// 将因果链特征格式化为 Phase B Prompt 可注入的结构化文本。
///
/// 格式:
/// - 因果网络概况（参与事件数、边数、最长链长度）
/// - 循环模式列表（如有）
/// - 因果边时延分布（扩展段，`latency_stats` 为空时不渲染）
/// - 情绪沿链走势（扩展段，`emotion_trend` 为空时不渲染）
/// - 解读提示
///
/// 参数:
/// - `features`: 因果链特征。
///
/// 返回:
/// - 格式化后的中文段落文本。若 chain_length=0 且无循环模式且无扩展特征，
///   返回空字符串。
pub fn format_causal_features_text(features: &CausalChainFeatures) -> String {
    if features.chain_length == 0
        && features.cyclic_patterns.is_empty()
        && features.latency_stats.is_empty()
        && features.emotion_trend.is_empty()
    {
        return String::new();
    }

    let mut text = String::new();
    text.push_str("## 因果链分析 (A8)\n\n");

    text.push_str(&format!(
        "因果网络概况: {} 个事件通过 {} 条因果关系连接",
        features.total_causal_events, features.total_causal_edges
    ));

    if features.chain_length > 0 {
        text.push_str(&format!("，最长因果链为 {} 跳。\n", features.chain_length));
        // 解读提示
        let driver_hint = if features.chain_length >= 3 {
            "长因果链提示用户可能是事件的\"主动驱动者\"（行为产生连锁影响）。"
        } else if features.chain_length >= 2 {
            "中等因果链提示用户行为有一定连锁效应。"
        } else {
            "短因果链提示用户行为影响较为局部。"
        };
        text.push_str(&format!("解读提示: {}\n", driver_hint));
    } else {
        text.push_str("。\n");
    }

    if !features.cyclic_patterns.is_empty() {
        text.push_str("\n**重复出现的行为脚本（循环模式）:**\n");
        for (i, pattern) in features.cyclic_patterns.iter().enumerate() {
            text.push_str(&format!(
                "  {}. \"{}\" — 出现 {} 次\n",
                i + 1,
                pattern.description,
                pattern.occurrences
            ));
        }
        text.push_str("注意: 循环模式指向稳定的行为脚本，应在性格推断中优先考虑。\n");
    }

    // ---- 扩展段: 因果边时延分布 ----
    if !features.latency_stats.is_empty() {
        let s = &features.latency_stats;
        text.push_str("\n**因果边时延分布:**\n");
        text.push_str(&format!(
            "  有效采样 {} 条边（剔除 {} 条时间缺失/负时延边）。\n",
            s.sampled_edge_count, s.excluded_edge_count
        ));
        let days = |ms: f64| ms / MS_PER_DAY as f64;
        if let (Some(mean), Some(median)) = (s.mean_ms, s.median_ms) {
            text.push_str(&format!(
                "  时延均值约 {:.1} 天，中位数约 {:.1} 天，",
                days(mean),
                days(median)
            ));
        }
        if let (Some(min), Some(max)) = (s.min_ms, s.max_ms) {
            text.push_str(&format!("范围 {:.1} ~ {:.1} 天。\n", days(min), days(max)));
        } else {
            text.push('\n');
        }
        text.push_str(&format!(
            "  分档: ≤1 天 {} 条、1-7 天 {} 条、>7 天 {} 条。\n",
            s.within_1d_count, s.within_7d_count, s.over_7d_count
        ));
    }

    // ---- 扩展段: 情绪沿链走势 ----
    if !features.emotion_trend.is_empty() {
        let t = &features.emotion_trend;
        text.push_str("\n**情绪沿链走势:**\n");
        text.push_str(&format!(
            "  沿最长因果链采样 {} 个事件节点。\n",
            t.sampled_node_count
        ));
        if let Some(mean) = t.mean_valence {
            text.push_str(&format!("  valence 均值 {:.2}，", mean));
        }
        if let Some(delta) = t.head_tail_delta {
            text.push_str(&format!("首末变化 {:.2}，", delta));
        }
        if let Some(slope) = t.linear_slope {
            text.push_str(&format!("每步趋势斜率 {:.3}，", slope));
        }
        text.push_str(&format!("正负翻转 {} 次。\n", t.polarity_flips));
        text.push_str(&format!("  情绪整体呈\"{}\"沿链演变。\n", t.direction));
    }

    text.push('\n');
    text
}
