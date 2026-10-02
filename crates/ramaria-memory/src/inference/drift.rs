//! crates/ramaria-memory/src/inference/drift.rs - 性格漂移检测
//!
//! 设计特点:
//! - C1: 1D Wasserstein 距离 + 蒙特卡洛置换检验动态阈值
//! - B=1000 锁定（不可配置），α=0.05 锁定
//! - 逐维度独立判定（valence/share），任一维度显著漂移 → 该分类标记"需重审"
//! - 方向补充: Δμ = μ_new - μ_old，漂移方向信息供 LLM 推断使用
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时
//! - samples-based 设计：输入为两组浮点数组（旧值和新值），由调用方从 MemoryEvent 提取

// =========================================================
// 配置类型
// =========================================================

/// Wasserstein 漂移检测配置。
///
/// 职责:
/// - 管理置换检验参数和显著性水平。
/// - 控制漂移检测是否启用（需恢复真实旧分布才可对比）。
///
/// 字段约定:
/// - `alpha`: 显著性水平，锁定 0.05。
/// - `n_permutations`: 置换次数，锁定 1000。
/// - `restore_real_distribution`: 是否从快照恢复真实旧分布；`false` 表示漂移检测整体关闭。
#[derive(Debug, Clone)]
pub struct DriftConfig {
    /// 显著性水平（锁定 0.05）
    pub alpha: f64,
    /// 置换检验次数（锁定 1000）
    pub n_permutations: usize,
    /// 是否从 `persona_cluster_snapshots` samples JSON 恢复真实旧分布。
    ///
    /// `true`（默认）: 漂移检测对比上一轮快照真实分布与当前事件分布。
    /// `false`: 漂移检测整体显式跳过（无真实旧分布可对比，不生成占位假数据）。
    pub restore_real_distribution: bool,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            n_permutations: 1000,
            restore_real_distribution: true,
        }
    }
}

impl From<ramaria_core::config::DriftConf> for DriftConfig {
    fn from(conf: ramaria_core::config::DriftConf) -> Self {
        Self {
            alpha: conf.alpha,
            n_permutations: conf.n_permutations,
            // 真实分布恢复由上层按 `InferenceUpgradeConfig.drift_restore_real_distribution`
            // 显式覆盖；此处保持默认开启，避免在未接线场景静默关闭漂移检测。
            restore_real_distribution: true,
        }
    }
}

// =========================================================
// 输出类型
// =========================================================

/// 单维度漂移检测结果。
///
/// 职责:
/// - 封装一个维度（valence 或 share）的漂移检测完整输出。
#[derive(Debug, Clone)]
pub struct DimensionDriftResult {
    /// 维度名称（"valence" / "share"）
    pub dimension: String,
    /// 观测到的 Wasserstein 距离
    pub wasserstein_distance: f64,
    /// 置换检验动态阈值（95 分位数）
    pub threshold: f64,
    /// 旧组加权均值
    pub mean_old: f64,
    /// 新组加权均值
    pub mean_new: f64,
    /// 均值漂移方向（Δμ = μ_new - μ_old）
    pub delta_mean: f64,
    /// 是否显著漂移（W > threshold）
    pub is_significant: bool,
    /// 旧组样本量
    pub n_old: usize,
    /// 新组样本量
    pub n_new: usize,
}

/// 单分类漂移检测结果。
///
/// 职责:
/// - 封装一个事件分类的完整漂移检测输出。
/// - 任一维度显著漂移时 `needs_review=true`。
///
/// - salience_drift: salience 维度漂移检测结果。
/// - confidence_drift: confidence 维度漂移检测结果。
#[derive(Debug, Clone)]
pub struct CategoryDriftResult {
    /// 分类标签
    pub category: String,
    /// valence 维度结果
    pub valence_drift: DimensionDriftResult,
    /// share 维度结果
    pub share_drift: DimensionDriftResult,
    /// salience 维度结果
    pub salience_drift: DimensionDriftResult,
    /// confidence 维度结果
    pub confidence_drift: DimensionDriftResult,
    /// 是否需要重审（任维度显著漂移）
    pub needs_review: bool,
}

/// 全局漂移检测汇总。
#[derive(Debug, Clone)]
pub struct DriftSummary {
    /// 逐分类检测结果
    pub categories: Vec<CategoryDriftResult>,
    /// 触发重审的分类数
    pub review_count: usize,
    /// 是否任一分类触发了漂移
    pub any_drift: bool,
    /// 因旧分布缺失/无判别信息/真实恢复未启用而未进入检测的候选分类数。
    ///
    /// 由上层编排（`detect_and_summarize_drift`）在按分类装配数据时累计；
    /// `run_drift_detection` 收到的数据已过滤，恒为 0。
    pub skipped_count: usize,
}

// =========================================================
// 1D Wasserstein 距离（闭式解）
// =========================================================

/// 计算两个样本集的 1D Wasserstein 距离。
///
/// 闭式解: W(p, q) = (1/n) · Σ|F_p^(-1)(i/n) - F_q^(-1)(i/n)|
/// 等价于: 对两个排序序列逐元素取绝对差后求均值。
///
/// 参数:
/// - `a`: 旧组样本值列表。
/// - `b`: 新组样本值列表。
///
/// 返回:
/// - Wasserstein 距离（≥ 0）。若任一组为空则返回 0.0。
pub fn wasserstein_1d(a: &[f64], b: &[f64]) -> f64 {
    let na = a.len();
    let nb = b.len();
    if na == 0 || nb == 0 {
        return 0.0;
    }

    let mut old_sorted = a.to_vec();
    let mut new_sorted = b.to_vec();
    old_sorted.sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    new_sorted.sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));

    // 使用线性插值在统一网格上计算距离
    // 取两组中较大者作为网格大小
    let n = na.max(nb);
    let mut total = 0.0;

    for i in 0..n {
        // 分位数位置 (i / n)
        let q = i as f64 / n as f64;
        let old_val = quantile(&old_sorted, q);
        let new_val = quantile(&new_sorted, q);
        total += (old_val - new_val).abs();
    }

    total / n as f64
}

/// 从已排序数组中取分位数值（线性插值）。
fn quantile(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    let pos = q * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;

    if lo >= n {
        return sorted[n - 1];
    }
    if hi >= n {
        return sorted[lo];
    }
    let frac = pos - pos.floor();
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

// =========================================================
// 蒙特卡洛置换检验
// =========================================================

/// 快速伪随机数生成器（Xorshift 变体，无外部 crate 依赖）。
///
/// 说明:
/// - 置换检验不需要密码学安全的随机性。
/// - 固定种子以保证可复现性。
struct XorShift {
    state: u64,
}

impl XorShift {
    fn new(seed: u64) -> Self {
        // 确保种子非零
        let state = if seed == 0 {
            0xDEAD_BEEF_CAFE_BABE
        } else {
            seed
        };
        Self { state }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// 生成 [0, n) 范围内的随机索引。
    fn next_index(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

/// 对合并池进行随机打乱（Fisher-Yates shuffle）。
fn shuffle_pool(pool: &mut [f64], rng: &mut XorShift) {
    let n = pool.len();
    for i in (1..n).rev() {
        let j = rng.next_index(i + 1);
        pool.swap(i, j);
    }
}

/// 执行蒙特卡洛置换检验，构建动态阈值。
///
/// 流程:
/// 1. 合并新旧两组数据为总池。
/// 2. 随机打乱后重新分成两组（保持原始大小）。
/// 3. 计算 Wasserstein 距离。
/// 4. 重复 B 次，得到零假设下的距离分布。
/// 5. 取 (1-α) 分位数作为动态阈值。
///
/// 参数:
/// - `a`: 旧组样本值。
/// - `b`: 新组样本值。
/// - `config`: 漂移检测配置。
///
/// 返回:
/// - (观测Wasserstein距离, 动态阈值, 置换距离列表)。
pub fn permutation_test(a: &[f64], b: &[f64], config: &DriftConfig) -> (f64, f64, Vec<f64>) {
    let na = a.len();
    let nb = b.len();
    let observed_w = wasserstein_1d(a, b);

    if na == 0 || nb == 0 {
        return (observed_w, 0.0, Vec::new());
    }

    // 合并总池
    let total = na + nb;
    let mut pool = Vec::with_capacity(total);
    pool.extend_from_slice(a);
    pool.extend_from_slice(b);

    // 固定种子以保证可复现
    let mut rng = XorShift::new(42);
    let mut permuted_distances = Vec::with_capacity(config.n_permutations);

    for _ in 0..config.n_permutations {
        shuffle_pool(&mut pool, &mut rng);

        // 分成两组（保持原始大小）
        let perm_a = &pool[..na];
        let perm_b = &pool[na..];
        let w = wasserstein_1d(perm_a, perm_b);
        permuted_distances.push(w);
    }

    // 排序后取 (1-α) 分位数
    permuted_distances
        .sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let threshold_idx = ((1.0 - config.alpha) * config.n_permutations as f64) as usize;
    let threshold_idx = threshold_idx.min(config.n_permutations - 1);
    let threshold = permuted_distances[threshold_idx];

    (observed_w, threshold, permuted_distances)
}

// =========================================================
// 加权均值
// =========================================================

/// 计算加权均值（带 salience 权重）。
///
/// 参数:
/// - `values`: 指标值列表。
/// - `weights`: salience 权重列表（需与 values 一一对应）。
fn weighted_mean_drift(values: &[f64], weights: &[f64]) -> f64 {
    let total_w: f64 = weights.iter().sum();
    if total_w <= 0.0 {
        return 0.0;
    }
    values.iter().zip(weights).map(|(v, w)| v * w).sum::<f64>() / total_w
}

// =========================================================
// 单维度漂移检测
// =========================================================

/// 对单个维度执行 Wasserstein 漂移检测。
///
/// 参数:
/// - `dimension`: 维度名（"valence"/"share"）。
/// - `old_values`: 旧组该维度的值列表。
/// - `new_values`: 新组该维度的值列表。
/// - `old_saliences`: 旧组各事件的 salience（用于加权均值）。
/// - `new_saliences`: 新组各事件的 salience（用于加权均值）。
/// - `config`: 漂移检测配置。
///
/// 返回:
/// - DimensionDriftResult。
pub fn detect_dimension_drift(
    dimension: &str,
    old_values: &[f64],
    new_values: &[f64],
    old_saliences: &[f64],
    new_saliences: &[f64],
    config: &DriftConfig,
) -> DimensionDriftResult {
    let (was_dist, threshold, _) = permutation_test(old_values, new_values, config);
    let mean_old = weighted_mean_drift(old_values, old_saliences);
    let mean_new = weighted_mean_drift(new_values, new_saliences);
    let delta_mean = mean_new - mean_old;
    let is_significant = was_dist > threshold && threshold > 1e-12;

    DimensionDriftResult {
        dimension: dimension.to_string(),
        wasserstein_distance: was_dist,
        threshold,
        mean_old,
        mean_new,
        delta_mean,
        is_significant,
        n_old: old_values.len(),
        n_new: new_values.len(),
    }
}

// =========================================================
// 逐分类漂移检测
// =========================================================

/// 单个分类的事件数据（用于漂移检测）。
///
/// - old_confidences / new_confidences: confidence 维度漂移检测。
#[derive(Debug, Clone)]
pub struct CategoryEventData {
    /// 分类标签
    pub category: String,
    /// 旧事件 valence 值
    pub old_valences: Vec<f64>,
    /// 旧事件 share 值
    pub old_shares: Vec<f64>,
    /// 旧事件 salience 值
    pub old_saliences: Vec<f64>,
    /// 旧事件 confidence 值
    pub old_confidences: Vec<f64>,
    /// 新事件 valence 值
    pub new_valences: Vec<f64>,
    /// 新事件 share 值
    pub new_shares: Vec<f64>,
    /// 新事件 salience 值
    pub new_saliences: Vec<f64>,
    /// 新事件 confidence 值
    pub new_confidences: Vec<f64>,
}

/// 对单个分类执行完整的漂移检测（四维度：valence / share / salience / confidence）。
///
/// 参数:
/// - `data`: 分类的新旧事件数据。
/// - `config`: 漂移检测配置。
///
/// 返回:
/// - CategoryDriftResult。
pub fn detect_category_drift(
    data: &CategoryEventData,
    config: &DriftConfig,
) -> CategoryDriftResult {
    let valence_drift = detect_dimension_drift(
        "valence",
        &data.old_valences,
        &data.new_valences,
        &data.old_saliences,
        &data.new_saliences,
        config,
    );
    let share_drift = detect_dimension_drift(
        "share",
        &data.old_shares,
        &data.new_shares,
        &data.old_saliences,
        &data.new_saliences,
        config,
    );
    let salience_drift = detect_dimension_drift(
        "salience",
        &data.old_saliences,
        &data.new_saliences,
        &data.old_saliences,
        &data.new_saliences,
        config,
    );
    let confidence_drift = detect_dimension_drift(
        "confidence",
        &data.old_confidences,
        &data.new_confidences,
        &data.old_saliences,
        &data.new_saliences,
        config,
    );

    let needs_review = valence_drift.is_significant
        || share_drift.is_significant
        || salience_drift.is_significant
        || confidence_drift.is_significant;

    CategoryDriftResult {
        category: data.category.clone(),
        valence_drift,
        share_drift,
        salience_drift,
        confidence_drift,
        needs_review,
    }
}

/// 对所有分类执行漂移检测。
///
/// 参数:
/// - `categories_data`: 各分类的新旧事件数据。
/// - `config`: 漂移检测配置。
///
/// 返回:
/// - DriftSummary。
pub fn run_drift_detection(
    categories_data: &[CategoryEventData],
    config: &DriftConfig,
) -> DriftSummary {
    let categories: Vec<CategoryDriftResult> = categories_data
        .iter()
        .map(|data| detect_category_drift(data, config))
        .collect();

    let review_count = categories.iter().filter(|c| c.needs_review).count();
    let any_drift = review_count > 0;

    DriftSummary {
        categories,
        review_count,
        any_drift,
        skipped_count: 0,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
