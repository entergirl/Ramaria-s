//! crates/ramaria-memory/src/chat/recent_state.rs - 近期状态（心情）判定模块
//!
//! 设计特点:
//! - 纯函数判定：窗口过滤 → 显著性加权效价 → 中性带方向判定，零 I/O
//! - 中性不注入：无样本 / 导向不明显时不在提示词产生任何输出（减少噪音）
//! - 加载降级：查询失败只记 warn 返回 None，不阻塞对话主流程
//! - 安全约束：日志只记 persona 与是否有判定结果，不记摘要文本

use ramaria_core::traits::StorageBackend;
use ramaria_core::types::MemoryL1;

/// 查询窗口内 L1 摘要条数上限（按最近优先取用）。
const RECENT_L1_LIMIT: u32 = 100;

/// 中性带半宽：窗口加权效价绝对值不超过该值视为中性（不注入）。
const NEUTRAL_BAND: f64 = 0.15;

/// 一天毫秒数（窗口换算）。
const DAY_MS: i64 = 86_400_000;

// =========================================================
// 近期心情
// =========================================================

/// 近期心情（最近记忆的效价时序方向）。
///
/// 变体:
/// - `Positive`: 窗口加权效价高于中性带（心情不错）；
/// - `Negative`: 窗口加权效价低于中性带（心情低落）。
///
/// 说明:
/// - 中性 / 无样本由 `Option::None` 表达（调用方不注入提示词）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecentMood {
    /// 正向（心情不错）
    Positive,
    /// 负向（心情低落）
    Negative,
}

/// 计算近期心情（窗口内 L1 效价的显著性加权方向）。
///
/// 流程:
/// 1. 窗口过滤：仅计入有效发生时间（`occurred_at_or_created`）在
///    `now - window_days` 之后的 L1；
/// 2. 窗口内无样本 → None；
/// 3. 加权效价 `Σ(valence × salience) / Σ(salience)`；权重和 ≤ 0 回退简单平均；
/// 4. `|加权效价| ≤ 0.15` → None（中性不注入），超出按符号判定方向。
///
/// 参数:
/// - `l1`: 候选 L1 摘要（顺序不参与计算）；
/// - `now`: 当前时间（Unix 毫秒）；
/// - `window_days`: 统计窗口（天）。
///
/// 返回:
/// - 方向明确的近期心情；无样本或中性时 `None`。
pub fn compute_recent_mood(l1: &[MemoryL1], now: i64, window_days: u32) -> Option<RecentMood> {
    let cutoff = now - i64::from(window_days) * DAY_MS;
    let window: Vec<&MemoryL1> = l1
        .iter()
        .filter(|m| m.occurred_at_or_created() >= cutoff)
        .collect();
    if window.is_empty() {
        return None;
    }

    // 显著性加权效价；权重和异常（全零）时回退简单平均
    let weight_sum: f64 = window.iter().map(|m| m.salience).sum();
    let weighted = if weight_sum > 0.0 {
        window.iter().map(|m| m.valence * m.salience).sum::<f64>() / weight_sum
    } else {
        window.iter().map(|m| m.valence).sum::<f64>() / window.len() as f64
    };

    if weighted > NEUTRAL_BAND {
        Some(RecentMood::Positive)
    } else if weighted < -NEUTRAL_BAND {
        Some(RecentMood::Negative)
    } else {
        None
    }
}

/// 从存储加载 persona 最近 L1 并判定近期心情。
///
/// 流程:
/// - 经 `list_recent_l1_by_persona` 取最近 100 条（不区分 absorbed 状态）；
/// - 经 [`compute_recent_mood`] 判定；查询失败只记 warn 并返回 None（不阻塞对话）。
///
/// 参数:
/// - `storage`: 存储后端；
/// - `persona_uid`: 目标人格；
/// - `window_days`: 统计窗口（天）。
///
/// 返回:
/// - 方向明确的近期心情；无样本 / 中性 / 查询失败时 `None`。
///
/// 安全约束:
/// - 日志只记 persona 与是否有判定结果，不记摘要文本。
pub(crate) async fn load(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    window_days: u32,
) -> Option<RecentMood> {
    let l1 = match storage
        .list_recent_l1_by_persona(persona_uid, RECENT_L1_LIMIT)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                persona_uid = persona_uid,
                error = %e,
                "加载近期 L1 失败，近期状态降级为空"
            );
            return None;
        }
    };
    let mood = compute_recent_mood(&l1, ramaria_core::types::now_ms(), window_days);
    tracing::debug!(
        persona_uid = persona_uid,
        present = mood.is_some(),
        "近期状态判定完成"
    );
    mood
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
