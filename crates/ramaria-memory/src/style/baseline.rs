//! crates/ramaria-memory/src/style/baseline.rs - 全局基线池与显著性检验模块
//!
//! 设计特点:
//! - 全局基线池：系统内全部 persona 已入库消息的归一化合并池（按 persona 归一）
//! - 池内每个 persona 存"频率摘要"（计数 / 有效字符数），全局频率 = 各 persona 平均
//! - 增量更新：封存时按 persona 替换旧贡献（O(1) 更新 + O(personas) 重算均值）
//! - 二项 z 检验：z = (f_p − f_g) / √(f_g(1−f_g)/n_p)；判定 |z|≥2 且频次≥5 且 n_p≥200
//! - 口癖词另加"相对超频比 > 2"（persona 频率 / 全局频率）
//! - 冷启动回退：池为空（无 persona 数据）时不检验、不生成规则（静默跳过）
//! - 安全约束：池内只存频率/计数，不含原文消息文本

use std::collections::HashMap;

use ramaria_core::config::StyleConfig;
use serde::{Deserialize, Serialize};

use super::stat::StyleStats;

// =========================================================
// 全局基线池
// =========================================================

/// 全局基线池：全部 persona 的归一化频率摘要集合。
///
/// 职责:
/// - 提供显著性检验所需的全局频率 f_g（各 persona 频率的算术平均）。
/// - 随封存增量更新：`update_persona` 替换该 persona 的贡献，不重复统计其他 persona。
///
/// 结构:
/// - `personas`: uid → 该 persona 的最新频率摘要。
/// - 全局频率（`global_*` 方法）由池内 persona 频率平均派生。
///
/// 更新策略（v3.1 §7.2 增量更新）:
/// - persona 内部统计采用全量重算（样本量 ≤ 数千时成本可控、准确优先）；
///   当 persona 数量增长影响性能时，可在 `update_persona` 内引入
///   滑动合并（旧统计 × 旧样本量 + 新统计 × 新样本量）/ 总样本量。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BaselinePool {
    personas: HashMap<String, PersonaFreq>,
}

/// 单个 persona 的频率摘要（基线池成员）。
///
/// 字段约定:
/// - 频率统一为"每 100 字出现次数"（展示口径，供相对超频比与模板使用）。
/// - 显著性检验在概率口径下进行（`StyleStats::freq` 提供 0..1 概率）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaFreq {
    /// 该 persona 的样本量（消息条数）
    pub sample_count: u32,
    /// 有效字符数（频率分母）
    pub total_chars: u32,
    /// 词 → 每 100 字频率（口癖词基线）
    pub word_freq: HashMap<String, f64>,
    /// 句长均值
    pub sentence_len_mean: f64,
    /// `||` 断句符每 100 字频率
    pub slash_freq: f64,
    /// 逗号每 100 字频率
    pub comma_freq: f64,
    /// 换行每 100 字频率
    pub newline_freq: f64,
    /// 感叹号每 100 字频率
    pub exclaim_freq: f64,
    /// 问号每 100 字频率
    pub question_freq: f64,
    /// 省略号每 100 字频率
    pub ellipsis_freq: f64,
    /// 括号每 100 字频率
    pub paren_freq: f64,
    /// 波浪号每 100 字频率
    pub tilde_freq: f64,
    /// 情感极性均值
    pub sentiment_mean: f64,
    /// 感叹词每 100 字频率
    pub interjection_freq: f64,
    /// 情感词典命中率（命中情感词的消息比例）
    pub sentiment_word_rate: f64,
    /// 词 → 每 100 字频率（话题词基线）
    pub topic_freq: HashMap<String, f64>,
}

impl BaselinePool {
    /// 创建空基线池（冷启动：无任何 persona 数据）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 池内 persona 数量。
    pub fn n_personas(&self) -> usize {
        self.personas.len()
    }

    /// 池是否为空（冷启动回退判断）。
    pub fn is_empty(&self) -> bool {
        self.personas.is_empty()
    }

    /// 按 persona 更新基线贡献（替换旧值，增量更新）。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    /// - `stats`: 该 persona 的最新统计（全量重算结果）。
    pub fn update_persona(&mut self, persona_uid: &str, stats: &StyleStats) {
        let freq = PersonaFreq::from_stats(stats);
        self.personas.insert(persona_uid.to_string(), freq);
    }

    /// 从池中移除 persona（persona 删除时清理）。
    pub fn remove_persona(&mut self, persona_uid: &str) {
        self.personas.remove(persona_uid);
    }

    /// 全局词频率（口癖词基线）：各 persona 每 100 字频率的平均。
    pub fn global_word_freq(&self, word: &str) -> f64 {
        average_freq(self.personas.values(), |p| p.word_freq.get(word).copied())
    }

    /// 全局话题词频率：各 persona 每 100 字频率的平均。
    pub fn global_topic_freq(&self, word: &str) -> f64 {
        average_freq(self.personas.values(), |p| p.topic_freq.get(word).copied())
    }

    /// 全局句长均值。
    pub fn global_sentence_len_mean(&self) -> f64 {
        average_freq(self.personas.values(), |p| Some(p.sentence_len_mean))
    }

    /// 全局断句符/标点/情感频率（每 100 字口径，统一访问）。
    pub fn global_metric(&self, key: MetricKey) -> f64 {
        average_freq(self.personas.values(), |p| Some(metric_value(p, key)))
    }

    /// 计算 persona 频率与全局频率的相对超频比（f_p / f_g；f_g=0 时返回 0）。
    pub fn relative_boost(f_p: f64, f_g: f64) -> f64 {
        if f_g <= 0.0 {
            return 0.0;
        }
        f_p / f_g
    }
}

impl PersonaFreq {
    /// 从 StyleStats 构造频率摘要（每 100 字口径）。
    fn from_stats(stats: &StyleStats) -> Self {
        let per100 = |count: u32| stats.per_100(count);
        let word_freq = stats
            .word_freq
            .iter()
            .map(|(w, c)| (w.clone(), per100(*c)))
            .collect();
        let topic_freq = stats
            .topic_freq
            .iter()
            .map(|(w, c)| (w.clone(), per100(*c)))
            .collect();
        Self {
            sample_count: stats.sample_count,
            total_chars: stats.total_chars,
            word_freq,
            sentence_len_mean: stats.sentence_len_mean,
            slash_freq: per100(stats.slash_count),
            comma_freq: per100(stats.comma_count),
            newline_freq: per100(stats.newline_count),
            exclaim_freq: per100(stats.exclaim_count),
            question_freq: per100(stats.question_count),
            ellipsis_freq: per100(stats.ellipsis_count),
            paren_freq: per100(stats.paren_count),
            tilde_freq: per100(stats.tilde_count),
            sentiment_mean: stats.sentiment_mean,
            interjection_freq: per100(stats.interjection_count),
            sentiment_word_rate: if stats.sample_count > 0 {
                stats.sentiment_word_messages as f64 / stats.sample_count as f64
            } else {
                0.0
            },
            topic_freq,
        }
    }
}

/// 基线指标键（全局频率统一访问）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKey {
    /// `||` 断句符
    Slash,
    /// 逗号
    Comma,
    /// 换行
    Newline,
    /// 感叹号
    Exclaim,
    /// 问号
    Question,
    /// 省略号
    Ellipsis,
    /// 括号
    Paren,
    /// 波浪号
    Tilde,
    /// 感叹词
    Interjection,
    /// 情感词典命中率（消息比例）
    SentimentWordRate,
    /// 情感极性均值
    SentimentMean,
}

fn metric_value(p: &PersonaFreq, key: MetricKey) -> f64 {
    match key {
        MetricKey::Slash => p.slash_freq,
        MetricKey::Comma => p.comma_freq,
        MetricKey::Newline => p.newline_freq,
        MetricKey::Exclaim => p.exclaim_freq,
        MetricKey::Question => p.question_freq,
        MetricKey::Ellipsis => p.ellipsis_freq,
        MetricKey::Paren => p.paren_freq,
        MetricKey::Tilde => p.tilde_freq,
        MetricKey::Interjection => p.interjection_freq,
        MetricKey::SentimentWordRate => p.sentiment_word_rate,
        MetricKey::SentimentMean => p.sentiment_mean,
    }
}

/// 各 persona 频率的算术平均（池为空时返回 0，即冷启动回退）。
fn average_freq<'a, F>(personas: impl Iterator<Item = &'a PersonaFreq>, get: F) -> f64
where
    F: Fn(&PersonaFreq) -> Option<f64>,
{
    let mut sum = 0.0;
    let mut n = 0usize;
    for p in personas {
        if let Some(v) = get(p) {
            sum += v;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

// =========================================================
// 显著性检验（二项 z 检验）
// =========================================================

/// 二项 z 检验。
///
/// 公式（v3.1 §7.2 / D-V17-003）:
/// ```text
/// z = (f_p − f_g) / √( f_g × (1 − f_g) / n_p )
/// ```
/// 其中 `f_p`/`f_g` 为概率形式（0..1）。
///
/// 边界:
/// - `f_g ≤ 0`（基线无该事件）→ 返回 0（无法检验，不显著）。
/// - `n_p = 0` → 返回 0。
/// - 分母为 0（f_g=1 或 f_g=0）→ 返回 0。
pub fn z_test(f_p: f64, f_g: f64, n_p: u32) -> f64 {
    if f_g <= 0.0 || f_g >= 1.0 || n_p == 0 {
        return 0.0;
    }
    let denom = (f_g * (1.0 - f_g) / n_p as f64).sqrt();
    if denom <= 0.0 {
        return 0.0;
    }
    (f_p - f_g) / denom
}

/// 显著性判定（三条件齐备）:
/// 1. `|z| ≥ z_critical`
/// 2. `频次 ≥ min_frequency`
/// 3. `n_p ≥ min_sample_count`
pub fn is_significant(z: f64, count: u32, n_p: u32, config: &StyleConfig) -> bool {
    z.abs() >= config.z_critical && count >= config.min_frequency && n_p >= config.min_sample_count
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> StyleConfig {
        StyleConfig::default()
    }

    fn sample_stats() -> StyleStats {
        StyleStats {
            sample_count: 200,
            total_chars: 2000,
            word_freq: vec![("哇塞".to_string(), 40)],
            topic_freq: vec![("电影".to_string(), 30)],
            sentence_len_mean: 8.0,
            slash_count: 100,
            comma_count: 80,
            newline_count: 10,
            exclaim_count: 120,
            question_count: 40,
            ellipsis_count: 20,
            paren_count: 10,
            tilde_count: 5,
            sentiment_mean: 0.3,
            sentiment_std: 0.2,
            sentiment_n: 200,
            interjection_count: 60,
            sentiment_word_messages: 100,
            ..Default::default()
        }
    }

    #[test]
    fn empty_pool_is_cold_start() {
        let pool = BaselinePool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.n_personas(), 0);
        assert_eq!(pool.global_word_freq("哇塞"), 0.0);
        assert_eq!(pool.global_metric(MetricKey::Exclaim), 0.0);
    }

    #[test]
    fn update_persona_replaces_contribution() {
        let mut pool = BaselinePool::new();
        let stats = sample_stats();
        pool.update_persona("char-0001", &stats);
        assert_eq!(pool.n_personas(), 1);
        assert!(
            (pool.global_word_freq("哇塞") - 2.0).abs() < 1e-9,
            "40/2000×100=2.0"
        );
        assert!((pool.global_metric(MetricKey::Exclaim) - 6.0).abs() < 1e-9);

        // 替换贡献：词频更新为 60 → 3.0
        let mut updated = stats.clone();
        updated.word_freq = vec![("哇塞".to_string(), 60)];
        pool.update_persona("char-0001", &updated);
        assert_eq!(pool.n_personas(), 1, "替换不新增 persona");
        assert!((pool.global_word_freq("哇塞") - 3.0).abs() < 1e-9);
    }

    #[test]
    fn global_freq_is_average_across_personas() {
        let mut pool = BaselinePool::new();
        let mut a = sample_stats();
        a.total_chars = 1000;
        a.exclaim_count = 100; // 每 100 字 10
        pool.update_persona("char-0001", &a);
        let mut b = sample_stats();
        b.total_chars = 1000;
        b.exclaim_count = 50; // 每 100 字 5
        pool.update_persona("char-0002", &b);
        assert_eq!(pool.n_personas(), 2);
        // 平均 (10 + 5) / 2 = 7.5
        assert!((pool.global_metric(MetricKey::Exclaim) - 7.5).abs() < 1e-9);
    }

    #[test]
    fn remove_persona_drops_contribution() {
        let mut pool = BaselinePool::new();
        pool.update_persona("char-0001", &sample_stats());
        pool.remove_persona("char-0001");
        assert!(pool.is_empty());
    }

    #[test]
    fn z_test_matches_hand_calculation() {
        // f_p=0.10, f_g=0.05, n_p=200
        // z = 0.05 / sqrt(0.05*0.95/200) = 0.05 / sqrt(0.0002375)
        let z = z_test(0.10, 0.05, 200);
        let inner: f64 = 0.05 * 0.95 / 200.0;
        let expected = 0.05 / inner.sqrt();
        assert!((z - expected).abs() < 1e-9);
        assert!(z > 2.0, "应显著：z={z}");
    }

    #[test]
    fn z_test_negative_direction() {
        let z = z_test(0.01, 0.05, 200);
        assert!(z < 0.0, "低于基线为负 z：{z}");
    }

    #[test]
    fn z_test_edge_cases_return_zero() {
        assert_eq!(z_test(0.1, 0.0, 200), 0.0, "全局频率为 0 → 不检验");
        assert_eq!(z_test(0.1, 0.05, 0), 0.0, "样本量为 0 → 不检验");
        assert_eq!(z_test(0.1, 1.0, 200), 0.0, "全局频率为 1 → 不检验");
    }

    #[test]
    fn is_significant_requires_all_three_conditions() {
        let cfg = config();
        // 全部满足 → 显著
        assert!(is_significant(2.5, 5, 200, &cfg));
        // |z| 不足
        assert!(!is_significant(1.9, 5, 200, &cfg));
        // 频次不足
        assert!(!is_significant(2.5, 4, 200, &cfg));
        // 样本量不足
        assert!(!is_significant(2.5, 5, 199, &cfg));
        // 方向：显著偏低同样满足（|z|）
        assert!(is_significant(-2.5, 5, 200, &cfg));
    }

    #[test]
    fn relative_boost_comparison() {
        // f_p / f_g = 3.0 > 2 → 口癖词候选
        let boost = BaselinePool::relative_boost(3.0, 1.0);
        assert!((boost - 3.0).abs() < 1e-9);
        // 全局频率为 0 → 0（不判超频）
        assert_eq!(BaselinePool::relative_boost(3.0, 0.0), 0.0);
    }

    #[test]
    fn persona_freq_converts_to_per_100() {
        let stats = sample_stats();
        let freq = PersonaFreq::from_stats(&stats);
        assert!((freq.exclaim_freq - 6.0).abs() < 1e-9, "120/2000×100=6.0");
        assert!((freq.slash_freq - 5.0).abs() < 1e-9, "100/2000×100=5.0");
        assert!((freq.sentiment_word_rate - 0.5).abs() < 1e-9, "100/200=0.5");
    }
}
