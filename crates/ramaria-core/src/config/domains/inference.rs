//! crates/ramaria-core/src/config/domains/inference.rs - L3 推断与画像升级配置
//!
//! 设计特点:
//! - 推断器 / 置信度 / 漂移 / 校准四组子配置各自提供缺省回退
//! - 画像升级开关按增量独立控制，全部关闭时回退旧版画像行为
//! - 纯配置结构，不读取文件或环境变量

use serde::{Deserialize, Serialize};

// =========================================================
// L3 推断配置
// =========================================================

/// L3 性格推断配置（Phase B + Phase C）。
///
/// 职责:
/// - 集中管理推断器、置信度更新、漂移检测和全量校准的参数。
/// - 所有字段均含合理默认值，无需手动配置即可运行。
///
/// 字段约定:
/// - `inferrer`: Phase B LLM 三步推断参数。
/// - `confidence`: Phase C 证据累积置信度参数。
/// - `drift`: Phase C Wasserstein 漂移检测参数。
/// - `calibration`: 定期全量校准触发参数。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InferenceConfig {
    /// Phase B 推断器配置
    #[serde(default)]
    pub inferrer: InferrerConf,
    /// Phase C 置信度配置
    #[serde(default)]
    pub confidence: ConfidenceConf,
    /// Phase C 漂移检测配置
    #[serde(default)]
    pub drift: DriftConf,
    /// 全量校准配置
    #[serde(default)]
    pub calibration: CalibrationConf,
    /// 画像升级开关。
    /// 独立配置开关：全部关闭时输出回退旧版行为。
    #[serde(default)]
    pub upgrade: InferenceUpgradeConfig,
}

/// Phase B 推断器配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferrerConf {
    /// LLM 生成温度（默认 0.3）
    #[serde(default = "default_inferrer_temperature")]
    pub temperature: f64,
    /// LLM 最大输出 tokens（默认 2048）
    #[serde(default = "default_inferrer_max_tokens")]
    pub max_tokens: u32,
    /// 小样本分类的证据阈值（默认 5.0）
    #[serde(default = "default_inferrer_low_evidence")]
    pub low_evidence_threshold: f64,
    /// 每步最大 tokens（默认 2048）
    #[serde(default = "default_inferrer_step_tokens")]
    pub step_max_tokens: u32,
}

fn default_inferrer_temperature() -> f64 {
    0.3
}
fn default_inferrer_max_tokens() -> u32 {
    2048
}
fn default_inferrer_low_evidence() -> f64 {
    5.0
}
fn default_inferrer_step_tokens() -> u32 {
    2048
}

impl Default for InferrerConf {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 2048,
            low_evidence_threshold: 5.0,
            step_max_tokens: 2048,
        }
    }
}

/// Phase C 置信度更新配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceConf {
    /// L2 层稳定性系数 S（默认 60，Ebbinghaus 遗忘曲线）
    #[serde(default = "default_confidence_stability")]
    pub stability_s: f64,
    /// 时间衰减保底值（默认 0.01）
    #[serde(default = "default_confidence_min_decay")]
    pub min_decay: f64,
}

fn default_confidence_stability() -> f64 {
    60.0
}
fn default_confidence_min_decay() -> f64 {
    0.01
}

impl Default for ConfidenceConf {
    fn default() -> Self {
        Self {
            stability_s: 60.0,
            min_decay: 0.01,
        }
    }
}

/// Phase C 漂移检测配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftConf {
    /// 显著性水平（锁定 0.05）
    #[serde(default = "default_drift_alpha")]
    pub alpha: f64,
    /// 置换检验次数（锁定 1000）
    #[serde(default = "default_drift_n_permutations")]
    pub n_permutations: usize,
}

fn default_drift_alpha() -> f64 {
    0.05
}
fn default_drift_n_permutations() -> usize {
    1000
}

impl Default for DriftConf {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            n_permutations: 1000,
        }
    }
}

/// 全量校准配置（可序列化版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationConf {
    /// 增量更新轮次阈值（默认 10）
    #[serde(default = "default_calibration_round")]
    pub round_threshold: u32,
    /// 事件量翻倍比例阈值（默认 2.0）
    #[serde(default = "default_calibration_doubling")]
    pub event_doubling_ratio: f64,
    /// 差异告警比例（默认 0.3）
    #[serde(default = "default_calibration_diff_alert")]
    pub diff_alert_ratio: f64,
}

fn default_calibration_round() -> u32 {
    10
}
fn default_calibration_doubling() -> f64 {
    2.0
}
fn default_calibration_diff_alert() -> f64 {
    0.3
}

impl Default for CalibrationConf {
    fn default() -> Self {
        Self {
            round_threshold: 10,
            event_doubling_ratio: 2.0,
            diff_alert_ratio: 0.3,
        }
    }
}

// =========================================================
// 画像升级配置
// =========================================================

/// 画像升级开关。
///
/// 职责:
/// - 独立控制画像升级的四个增量（跨版本阈值 0.85 / 冷启动先验 / 漂移真实分布 /
///   因果链时延与情绪走势扩展特征）。
/// - 全部关闭时画像输出回退旧版行为。
///
/// 兼容性说明:
/// - 每个开关默认开启。
/// - struct 级 `#[serde(default)]`：`[inference.upgrade]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceUpgradeConfig {
    /// 跨版本簇匹配阈值是否使用 0.85。
    /// `false` → 回退旧值 0.75。
    pub cross_version_threshold_085: bool,
    /// 冷启动先验是否使用跨用户经验分布。
    /// `false` → 回退当前 persona 内先验。
    pub cold_start_cross_user_prior: bool,
    /// 漂移检测是否从 `persona_cluster_snapshots` 恢复真实旧分布并执行检测。
    /// `false` → 漂移检测整体显式跳过（无真实旧分布可对比，不生成占位假数据）。
    pub drift_restore_real_distribution: bool,
    /// Phase B 因果链是否注入"时延分布 + 情绪沿链走势"扩展段。
    /// `false` → 回退 v1.7 仅注入链长 / 循环模式（`extract_causal_features`）。
    pub causal_latency_emotion_trend: bool,
}

impl Default for InferenceUpgradeConfig {
    /// 创建默认画像升级配置。
    ///
    /// 返回:
    /// - 四个增量开关默认开启。
    fn default() -> Self {
        Self {
            cross_version_threshold_085: true,
            cold_start_cross_user_prior: true,
            drift_restore_real_distribution: true,
            causal_latency_emotion_trend: true,
        }
    }
}
