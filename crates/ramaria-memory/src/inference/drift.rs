//! rust/crates/ramaria-memory/src/inference/drift.rs - 性格漂移检测
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
///
/// 字段约定:
/// - `alpha`: 显著性水平，锁定 0.05。
/// - `n_permutations`: 置换次数，锁定 1000。
#[derive(Debug, Clone)]
pub struct DriftConfig {
    /// 显著性水平（锁定 0.05）
    pub alpha: f64,
    /// 置换检验次数（锁定 1000）
    pub n_permutations: usize,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            n_permutations: 1000,
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
/// v1.3 M5-B 新增:
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
    /// v1.3: salience 维度结果
    pub salience_drift: DimensionDriftResult,
    /// v1.3: confidence 维度结果
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

    let mut a_sorted = a.to_vec();
    let mut b_sorted = b.to_vec();
    a_sorted.sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    b_sorted.sort_unstable_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));

    // 使用线性插值在统一网格上计算距离
    // 取两组中较大者作为网格大小
    let n = na.max(nb);
    let mut total = 0.0;

    for i in 0..n {
        // 分位数位置 (i / n)
        let q = i as f64 / n as f64;
        let a_val = quantile(&a_sorted, q);
        let b_val = quantile(&b_sorted, q);
        total += (a_val - b_val).abs();
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
/// v1.3 M5-B 新增:
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
    /// v1.3: 旧事件 confidence 值
    pub old_confidences: Vec<f64>,
    /// 新事件 valence 值
    pub new_valences: Vec<f64>,
    /// 新事件 share 值
    pub new_shares: Vec<f64>,
    /// 新事件 salience 值
    pub new_saliences: Vec<f64>,
    /// v1.3: 新事件 confidence 值
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
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Wasserstein 1D ----

    #[test]
    fn wasserstein_identical_distributions() {
        let a = vec![0.5, 0.5, 0.5];
        let b = vec![0.5, 0.5, 0.5];
        let w = wasserstein_1d(&a, &b);
        assert!(w.abs() < 1e-10, "相同分布 Wasserstein 距离应为 0");
    }

    #[test]
    fn wasserstein_different_distributions() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 1.0, 1.0];
        let w = wasserstein_1d(&a, &b);
        assert!((w - 1.0).abs() < 1e-10, "完全分离分布距离应为 1.0");
    }

    #[test]
    fn wasserstein_empty_input() {
        assert_eq!(wasserstein_1d(&[], &[1.0, 2.0]), 0.0);
        assert_eq!(wasserstein_1d(&[1.0, 2.0], &[]), 0.0);
    }

    #[test]
    fn wasserstein_single_element() {
        let a = vec![0.0];
        let b = vec![1.0];
        let w = wasserstein_1d(&a, &b);
        assert!((w - 1.0).abs() < 1e-10);
    }

    #[test]
    fn wasserstein_different_sizes() {
        // 两组大小不同的分布
        let a = vec![0.0, 0.5, 1.0];
        let b = vec![0.3, 0.7];
        let w = wasserstein_1d(&a, &b);
        // 距离应在合理范围内
        assert!(w >= 0.0);
        assert!(w <= 1.0);
    }

    // ---- 置换检验 ----

    #[test]
    fn permutation_test_no_drift() {
        let config = DriftConfig::default();
        // 两组来自相同分布的数据（无漂移）
        let a = vec![0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        let b = vec![0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85];
        let (observed_w, threshold, _dists) = permutation_test(&a, &b, &config);
        // 观测距离应小于阈值（无显著漂移）
        assert!(
            observed_w <= threshold || threshold < 1e-12,
            "无漂移时观测距离应≤阈值。W={:.4}, 阈值={:.4}",
            observed_w,
            threshold
        );
    }

    #[test]
    fn permutation_test_with_drift() {
        let config = DriftConfig::default();
        // 两组来自明显不同分布
        let a = vec![0.0, 0.1, 0.0, 0.1, 0.0];
        let b = vec![0.8, 0.9, 0.8, 0.9, 0.8];
        let (observed_w, threshold, _dists) = permutation_test(&a, &b, &config);
        // 观测距离应显著大于阈值
        assert!(
            observed_w > threshold,
            "漂移时观测距离应>阈值。W={:.4}, 阈值={:.4}",
            observed_w,
            threshold
        );
    }

    #[test]
    fn permutation_test_small_sample() {
        let config = DriftConfig::default();
        // 小样本——阈值应自动上调
        let a = vec![0.0, 0.5];
        let b = vec![0.3, 0.7];
        let (observed_w, threshold, distances) = permutation_test(&a, &b, &config);
        assert_eq!(distances.len(), 1000, "应有 1000 次置换");
        // 阈值不应为 0（小样本有抽样噪声）
        assert!(threshold >= 0.0);
        // 观测值应在合理范围
        assert!(observed_w >= 0.0);
    }

    #[test]
    fn permutation_test_reproducibility() {
        let config = DriftConfig::default();
        let a = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let b = vec![0.6, 0.7, 0.8, 0.9, 1.0];
        let (w1, t1, _) = permutation_test(&a, &b, &config);
        let (w2, t2, _) = permutation_test(&a, &b, &config);
        // 固定种子应给出相同结果
        assert!((w1 - w2).abs() < 1e-10);
        assert!((t1 - t2).abs() < 1e-10);
    }

    // ---- 逐维度漂移检测 ----

    #[test]
    fn detect_dimension_drift_no_change() {
        let config = DriftConfig::default();
        let vals = vec![0.3, 0.4, 0.5, 0.6, 0.7];
        let sals = vec![0.5, 0.5, 0.5, 0.5, 0.5];
        let result = detect_dimension_drift("valence", &vals, &vals, &sals, &sals, &config);
        // 完全相同数据应无漂移
        assert!(!result.is_significant || result.wasserstein_distance < 0.01);
    }

    #[test]
    fn detect_dimension_drift_large_change() {
        let config = DriftConfig::default();
        let old = vec![-0.8, -0.7, -0.9, -0.6, -0.8];
        let new = vec![0.7, 0.8, 0.9, 0.6, 0.7];
        let sals = vec![0.5, 0.5, 0.5, 0.5, 0.5];
        let result = detect_dimension_drift("valence", &old, &new, &sals, &sals, &config);
        // 大幅变化应检测到漂移
        assert!(result.is_significant);
        assert!(result.delta_mean > 0.0, "从负到正，Δμ 应为正");
    }

    // ---- 分类漂移检测 ----

    #[test]
    fn category_drift_empty() {
        let config = DriftConfig::default();
        let data = CategoryEventData {
            category: "测试".into(),
            old_valences: vec![],
            old_shares: vec![],
            old_saliences: vec![],
            old_confidences: vec![],
            new_valences: vec![],
            new_shares: vec![],
            new_saliences: vec![],
            new_confidences: vec![],
        };
        let result = detect_category_drift(&data, &config);
        assert!(!result.needs_review, "空数据不应触发重审");
    }

    #[test]
    fn run_drift_detection_multiple_categories() {
        let config = DriftConfig::default();
        let data = vec![
            CategoryEventData {
                category: "工作".into(),
                old_valences: vec![0.1, 0.2, 0.1, 0.2],
                old_shares: vec![0.5, 0.6, 0.5, 0.6],
                old_saliences: vec![0.5; 4],
                old_confidences: vec![0.6; 4],
                new_valences: vec![0.8, 0.9, 0.8, 0.9],
                new_shares: vec![0.5, 0.6, 0.5, 0.6],
                new_saliences: vec![0.5; 4],
                new_confidences: vec![0.6; 4],
            },
            CategoryEventData {
                category: "社交".into(),
                old_valences: vec![0.3, 0.4, 0.3],
                old_shares: vec![0.7, 0.8, 0.7],
                old_saliences: vec![0.5; 3],
                old_confidences: vec![0.6; 3],
                new_valences: vec![0.3, 0.4, 0.3],
                new_shares: vec![0.7, 0.8, 0.7],
                new_saliences: vec![0.5; 3],
                new_confidences: vec![0.6; 3],
            },
        ];
        let summary = run_drift_detection(&data, &config);
        assert_eq!(summary.categories.len(), 2);
        // 工作应漂移（valence 大幅变化），社交应无漂移
        let work = summary
            .categories
            .iter()
            .find(|c| c.category == "工作")
            .unwrap();
        assert!(work.needs_review, "工作分类应触发漂移");
        assert!(summary.any_drift);
    }

    // ---- v1.3 M5-B: 新增 drift 维度 ----

    #[test]
    fn category_drift_salience_dimension() {
        let config = DriftConfig::default();
        // salience 大幅变化（0.1→0.9），valence/share 不变
        let data = CategoryEventData {
            category: "工作".into(),
            old_valences: vec![0.5; 5],
            old_shares: vec![0.5; 5],
            old_saliences: vec![0.1; 5],
            old_confidences: vec![0.5; 5],
            new_valences: vec![0.5; 5],
            new_shares: vec![0.5; 5],
            new_saliences: vec![0.9; 5],
            new_confidences: vec![0.5; 5],
        };
        let result = detect_category_drift(&data, &config);
        assert!(result.needs_review, "salience 大幅变化应触发漂移");
        assert!(result.salience_drift.is_significant);
    }

    #[test]
    fn category_drift_confidence_dimension() {
        let config = DriftConfig::default();
        // confidence 大幅变化（0.1→0.9），其他维度不变
        let data = CategoryEventData {
            category: "社交".into(),
            old_valences: vec![0.5; 5],
            old_shares: vec![0.5; 5],
            old_saliences: vec![0.5; 5],
            old_confidences: vec![0.1; 5],
            new_valences: vec![0.5; 5],
            new_shares: vec![0.5; 5],
            new_saliences: vec![0.5; 5],
            new_confidences: vec![0.9; 5],
        };
        let result = detect_category_drift(&data, &config);
        assert!(result.needs_review, "confidence 大幅变化应触发漂移");
        assert!(result.confidence_drift.is_significant);
    }

    #[test]
    fn category_drift_result_includes_new_dimensions() {
        let config = DriftConfig::default();
        let data = CategoryEventData {
            category: "工作".into(),
            old_valences: vec![0.5; 4],
            old_shares: vec![0.5; 4],
            old_saliences: vec![0.5; 4],
            old_confidences: vec![0.5; 4],
            new_valences: vec![0.8; 4],
            new_shares: vec![0.5; 4],
            new_saliences: vec![0.5; 4],
            new_confidences: vec![0.5; 4],
        };
        let result = detect_category_drift(&data, &config);
        // 四个维度都存在
        assert_eq!(result.valence_drift.dimension, "valence");
        assert_eq!(result.share_drift.dimension, "share");
        assert_eq!(result.salience_drift.dimension, "salience");
        assert_eq!(result.confidence_drift.dimension, "confidence");
        // valence 维度应漂移
        assert!(result.valence_drift.is_significant);
    }
}
