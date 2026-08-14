//! crates/ramaria-app/src/eta.rs - 导入进度分层 EMA 预估模块（v1.5 I 项）
//!
//! 设计特点:
//! - 纯计算模块（无 I/O、无异步），便于单元测试与在导入后台任务中复用。
//! - 按阶段（L1/L2/L3）分别维护 EMA 单次耗时：剩余时间 = Σ(各阶段剩余量 × EMA 单次耗时)。
//! - EMA 平滑系数 α 默认 0.3（对波动较大的导入耗时适度平滑，样本不足时直接采用首个样本）。
//! - 首次运行无历史（无任何样本）→ 返回 None，由调用方回退线性估算
//!   （`linear_remaining`：elapsed / total_done × total_remaining）。
//! - 各阶段进度通过 `update` 喂入：每次收到后端 import-progress 事件调用一次。
//!
//! 与前端的分工:
//! - 后端：阶段总量统计（L1=session×persona、L2/L3 在线估算）由调用方（import_cmd）
//!   喂入 `update`；`remaining_seconds` 计算结果随 import-progress 事件下发（`eta_seconds`）。
//! - 前端：优先展示后端 `eta_seconds`；缺失时回退既有线性估算（降级路径）。
//!
//! 边界与降级:
//! - 总量未知（total=0）的阶段不参与剩余时间计算（避免除零/无意义估算）。
//! - 已完成量 ≥ 总量 → 该阶段剩余 0。
//! - 无任何 EMA 样本 → None（调用方线性兜底）。
//! - 单阶段样本不足（1~2 个）→ 直接用样本均值（首个样本即采用，后续平滑）。

/// 导入管线阶段。
///
/// 与 `import-progress` 事件的 phase 字符串一一对应（l1/l2/l3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    /// L1 会话摘要生成（调用数可预知 = session × persona）
    L1,
    /// L2 事件提取（聚类簇在线估算）
    L2,
    /// L3 性格画像推断（固定阶段数）
    L3,
}

impl PhaseKind {
    /// 事件 phase 字符串（与前端契约一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            PhaseKind::L1 => "l1",
            PhaseKind::L2 => "l2",
            PhaseKind::L3 => "l3",
        }
    }

    /// 全部阶段（按执行顺序）。
    pub const ALL: [PhaseKind; 3] = [PhaseKind::L1, PhaseKind::L2, PhaseKind::L3];
}

/// 单阶段 EMA 状态。
#[derive(Debug, Clone)]
pub struct PhaseEma {
    /// 阶段
    pub kind: PhaseKind,
    /// 阶段预计总量（0 = 未知，不参与估算）
    pub total: usize,
    /// 已完成量
    pub done: usize,
    /// EMA 单次耗时（秒/项），None = 尚无样本
    pub ema_seconds_per_item: Option<f64>,
    /// 已累计样本数
    pub samples: u32,
}

impl PhaseEma {
    fn new(kind: PhaseKind) -> Self {
        Self {
            kind,
            total: 0,
            done: 0,
            ema_seconds_per_item: None,
            samples: 0,
        }
    }
}

/// 分层 EMA 剩余时间估算器。
///
/// 用法:
/// ```
/// use ramaria_app::eta::{EtaEstimator, PhaseKind};
/// let mut est = EtaEstimator::new();
/// // 每次收到 import-progress 事件时喂入当前阶段进度（累计秒数）
/// est.update(PhaseKind::L1, 5, 10, 30.0);
/// est.update(PhaseKind::L1, 10, 10, 55.0);
/// assert!(est.remaining_seconds().is_some());
/// ```
#[derive(Debug, Clone)]
pub struct EtaEstimator {
    /// EMA 平滑系数（0.0 < α ≤ 1.0）
    alpha: f64,
    /// 三阶段状态
    phases: [PhaseEma; 3],
}

impl Default for EtaEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl EtaEstimator {
    /// 创建估算器（α=0.3）。
    pub fn new() -> Self {
        Self::with_alpha(0.3)
    }

    /// 创建估算器并指定平滑系数。
    ///
    /// 参数:
    /// - `alpha`: EMA 平滑系数，`0.0 < alpha ≤ 1.0`（1.0 = 完全跟随最新样本，不做平滑）。
    pub fn with_alpha(alpha: f64) -> Self {
        assert!(
            alpha > 0.0 && alpha <= 1.0,
            "EMA 平滑系数须满足 0 < alpha <= 1，实际 {alpha}"
        );
        Self {
            alpha,
            phases: [
                PhaseEma::new(PhaseKind::L1),
                PhaseEma::new(PhaseKind::L2),
                PhaseEma::new(PhaseKind::L3),
            ],
        }
    }

    /// 更新某阶段进度并刷新该阶段的 EMA 单次耗时。
    ///
    /// 参数:
    /// - `phase`: 阶段。
    /// - `done`: 该阶段已完成量（调用方以事件 current 喂入）。
    /// - `total`: 该阶段预计总量（调用方以事件 total/阶段预计字段喂入，0 = 未知）。
    /// - `elapsed_secs`: 自导入开始到本次更新的累计秒数（单调递增）。
    ///
    /// EMA 更新规则:
    /// - `done == 0` 或 `elapsed_secs <= 0` → 仅记录进度，不更新 EMA（无有效样本）。
    /// - 单次耗时 = elapsed_secs / done（阶段内累计平均，对波动自然平滑）。
    /// - 首个样本直接采用；后续样本 `ema = α·新值 + (1−α)·旧值`。
    pub fn update(&mut self, phase: PhaseKind, done: usize, total: usize, elapsed_secs: f64) {
        let idx = phase_index(phase);
        let p = &mut self.phases[idx];
        p.done = done;
        p.total = total;

        if done > 0 && elapsed_secs > 0.0 {
            let item_secs = elapsed_secs / done as f64;
            p.ema_seconds_per_item = Some(match p.ema_seconds_per_item {
                Some(prev) => self.alpha * item_secs + (1.0 - self.alpha) * prev,
                None => item_secs, // 首个样本直接采用
            });
            p.samples += 1;
        }
    }

    /// 估算剩余总秒数。
    ///
    /// 算法（§2.4）:
    /// `剩余 = Σ(各阶段剩余量 × 该阶段 EMA 单次耗时)`
    ///
    /// 规则:
    /// - 总量未知（total=0）的阶段不参与（避免猜测）。
    /// - 尚无任何 EMA 样本（首次运行/样本不足）→ `None`，调用方回退线性估算。
    /// - 样本不足指"没有任何阶段有样本"；单个阶段有样本即开始估算
    ///   （其余阶段后续喂入后自动纳入，估算随进度自然收敛）。
    ///
    /// 返回:
    /// - `Some(秒数)`：可估算（至少一个阶段有样本且总量已知）。
    /// - `None`：无任何样本，无法估算（调用方线性兜底）。
    pub fn remaining_seconds(&self) -> Option<f64> {
        let mut total: f64 = 0.0;
        let mut has_sample = false;
        for p in &self.phases {
            if p.total == 0 {
                continue; // 总量未知，跳过
            }
            let remaining = p.total.saturating_sub(p.done) as f64;
            if let Some(ema) = p.ema_seconds_per_item {
                total += remaining * ema;
                has_sample = true;
            }
        }
        if has_sample {
            Some(total.max(0.0))
        } else {
            None
        }
    }

    /// 各阶段状态快照（供调试/测试断言）。
    pub fn phases(&self) -> &[PhaseEma; 3] {
        &self.phases
    }
}

/// 阶段 → 数组索引（L1=0, L2=1, L3=2）。
fn phase_index(kind: PhaseKind) -> usize {
    match kind {
        PhaseKind::L1 => 0,
        PhaseKind::L2 => 1,
        PhaseKind::L3 => 2,
    }
}

/// 线性速率估算（首次运行/无样本兜底）。
///
/// 公式: `剩余秒数 = elapsed / total_done × total_remaining`。
///
/// 规则:
/// - `total_done == 0` 或 `elapsed_secs <= 0` → None（无速率可算）。
/// - `total_remaining == 0` → Some(0.0)（已无剩余，即将完成）。
///
/// 参数:
/// - `elapsed_secs`: 已用秒数。
/// - `total_done`: 已完成总量。
/// - `total_remaining`: 剩余总量。
pub fn linear_remaining(
    elapsed_secs: f64,
    total_done: usize,
    total_remaining: usize,
) -> Option<f64> {
    if total_remaining == 0 {
        return Some(0.0);
    }
    if total_done == 0 || elapsed_secs <= 0.0 {
        return None;
    }
    let rate = elapsed_secs / total_done as f64; // 每项秒数
    Some(rate * total_remaining as f64)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_as_str_matches_event_contract() {
        assert_eq!(PhaseKind::L1.as_str(), "l1");
        assert_eq!(PhaseKind::L2.as_str(), "l2");
        assert_eq!(PhaseKind::L3.as_str(), "l3");
        assert_eq!(PhaseKind::ALL.len(), 3);
    }

    /// 无任何样本 → None（首次运行线性兜底路径）。
    #[test]
    fn no_samples_returns_none() {
        let mut est = EtaEstimator::new();
        // 有总量但从未 update → 无样本
        est.update(PhaseKind::L1, 0, 10, 5.0);
        assert!(est.remaining_seconds().is_none());
    }

    /// 单阶段 EMA：剩余 = 剩余量 × EMA 单次耗时。
    #[test]
    fn single_phase_remaining_uses_ema() {
        let mut est = EtaEstimator::new();
        // 2 项用了 10 秒 → 单次 5 秒；已 2 项，剩 8 项 → 40 秒
        est.update(PhaseKind::L1, 2, 10, 10.0);
        let rem = est.remaining_seconds().expect("应有估算");
        assert!((rem - 40.0).abs() < 1e-6, "剩余应约 40 秒，实际 {rem}");
    }

    /// 分层 EMA：各阶段剩余相加（L1 剩 8 项×5s + L2 剩 3 项×12s = 76s）。
    #[test]
    fn layered_remaining_sums_across_phases() {
        let mut est = EtaEstimator::new();
        est.update(PhaseKind::L1, 2, 10, 10.0); // 单次 5s，剩 8 → 40s
        est.update(PhaseKind::L2, 1, 4, 12.0); // 单次 12s，剩 3 → 36s
        let rem = est.remaining_seconds().expect("应有估算");
        assert!((rem - 76.0).abs() < 1e-6, "剩余应约 76 秒，实际 {rem}");
    }

    /// EMA 平滑：后续样本按 α 加权（α=0.5：prev=4s, new=8s → 6s）。
    #[test]
    fn ema_smoothing_blends_samples() {
        let mut est = EtaEstimator::with_alpha(0.5);
        est.update(PhaseKind::L1, 1, 10, 4.0); // 首个样本 4s
        est.update(PhaseKind::L1, 2, 10, 16.0); // 累计单次 8s → EMA = 0.5*8 + 0.5*4 = 6s
        let ema = est.phases()[0].ema_seconds_per_item.expect("应有 EMA");
        assert!((ema - 6.0).abs() < 1e-6, "EMA 应约 6s，实际 {ema}");
        // 剩 8 项 × 6s = 48s
        let rem = est.remaining_seconds().expect("应有估算");
        assert!((rem - 48.0).abs() < 1e-6, "剩余应约 48 秒，实际 {rem}");
    }

    /// 阶段完成（done == total）→ 剩余 0，不贡献时间。
    #[test]
    fn completed_phase_contributes_zero() {
        let mut est = EtaEstimator::new();
        est.update(PhaseKind::L1, 10, 10, 100.0); // L1 完成
        est.update(PhaseKind::L2, 1, 5, 110.0); // L2 单次 110s
        let rem = est.remaining_seconds().expect("应有估算");
        assert!(
            (rem - 440.0).abs() < 1e-6,
            "L1 剩余 0，剩余应约 440 秒，实际 {rem}"
        );
    }

    /// 总量未知（total=0）阶段不参与估算。
    #[test]
    fn unknown_total_phase_skipped() {
        let mut est = EtaEstimator::new();
        est.update(PhaseKind::L1, 2, 10, 10.0); // 单次 5s
        est.update(PhaseKind::L2, 0, 0, 10.0); // 总量未知 → 跳过
        let rem = est.remaining_seconds().expect("应有估算");
        assert!((rem - 40.0).abs() < 1e-6, "剩余应仅含 L1，实际 {rem}");
    }

    /// 样本不足（仅 L2 有样本、L1/L3 无）→ 仍可估算（后续阶段喂入后收敛）。
    #[test]
    fn partial_samples_still_estimates() {
        let mut est = EtaEstimator::new();
        est.update(PhaseKind::L1, 0, 10, 0.0); // 无样本
        est.update(PhaseKind::L3, 1, 2, 30.0); // L3 单次 30s
        let rem = est.remaining_seconds().expect("有样本阶段应可估算");
        assert!((rem - 30.0).abs() < 1e-6, "仅 L3 剩 1 项 × 30s，实际 {rem}");
    }

    /// 线性兜底：无样本 → 用 elapsed/done×remaining。
    #[test]
    fn linear_fallback_basic() {
        // 10 秒完成 5 项，剩 5 项 → 10 秒
        let rem = linear_remaining(10.0, 5, 5).expect("应有估算");
        assert!((rem - 10.0).abs() < 1e-6);
    }

    /// 线性兜底边界：剩余为 0 → Some(0)；无完成量 → None。
    #[test]
    fn linear_fallback_boundaries() {
        assert_eq!(linear_remaining(10.0, 5, 0), Some(0.0), "无剩余应返回 0");
        assert!(linear_remaining(10.0, 0, 5).is_none(), "无完成量无法估算");
        assert!(linear_remaining(0.0, 5, 5).is_none(), "无耗时无法估算");
    }

    /// 进度回退防御：done 超过 total 时按已完成处理（saturating 不溢出）。
    #[test]
    fn done_exceeding_total_is_clamped() {
        let mut est = EtaEstimator::new();
        est.update(PhaseKind::L1, 12, 10, 60.0); // done > total（事件乱序防御）
        let rem = est.remaining_seconds().expect("应有估算");
        assert!(rem < 1e-9, "剩余量应钳制为 0，实际 {rem}");
    }
}
