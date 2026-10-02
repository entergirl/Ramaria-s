//! crates/ramaria-core/src/config/domains.rs - Ramaria 领域能力配置模块
//!
//! 设计特点:
//! - 定义 L3 推断、画像升级与置信度/漂移/校准配置
//! - 定义示例选择与行为规则配置
//! - 定义知识、表达层风格与反馈配置
//! - 各推断子配置提供 serde 缺省回退
//! - 支持 serde，各配置组提供稳定默认值

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

// =========================================================
// examples 配置（v1.4 新增）
// =========================================================

/// examples（Few-shot 示例激活）配置。
///
/// 职责:
/// - 控制会话关闭时的回复对抽取、评分轮换与兜底注入。
/// - `max_examples` 与既有 `list_selected` 的 LIMIT 保持一致。
///
/// 说明:
/// - `enabled=false` 时行为回退 v1.3（读侧通道保留，写侧不激活）。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：`[examples]` 表只写部分键时回退默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExamplesConfig {
    /// 是否启用 examples 写侧激活（抽取/入库/轮换/兜底注入）。
    pub enabled: bool,
    /// 注入时的最大示例条数。
    pub max_examples: u32,
}

impl Default for ExamplesConfig {
    /// 创建默认 examples 配置。
    ///
    /// 返回:
    /// - 启用，最多注入 5 条示例（与既有查询 LIMIT 一致）。
    fn default() -> Self {
        Self {
            enabled: true,
            max_examples: 5,
        }
    }
}

// =========================================================
// 行为层配置
// =========================================================

/// 行为模型学习与驱动配置（`[behavior]` 配置组）。
///
/// 职责:
/// - 集中管理行为层全链路参数：聚类与规则生成、情境路由、增量更新。
/// - `enabled=false` 时行为层全链路关闭（不学习/不路由）。
///
/// 降级链:
/// - 聚类参数（θ_nb / min_cluster_size / θ_join / β1 / β2）按初值推进，
///   全部标注「待实证」——探针工具链定稿后回填，参数不可用时回退本默认值。
/// - embedding 不可用 → 双通道向量通道关闭，退化为纯关键词 Jaccard 通道（β=0）。
///
/// 字段约定:
/// - `theta_nb`: 密度聚类邻域相似度阈值（待实证：初值 0.65，v3.1 建议真实数据 P50~P75）。
/// - `beta1` + `beta2`: 双通道融合权重，约束 β1 + β2 ≤ 1（关键词通道 = 1 − β1 − β2）。
/// - `theta_route`: 路由阈值，全部候选低于此值 → 不注入（静默降级）。
/// - `top_n`: 路由 Top-N 合并上限（主规则完整注入 + 次规则仅合并 avoid/params）。
/// - `min_evidence`: 证据量门槛，簇内有效样本量 < 此值 → 不生成规则文本（仅参数）。
/// - `min_n_eff`: 有效样本量门槛（salience 加权），< 此值 → 降级候选规则。
/// - `valence_std_limit`: 簇内 valence 标准差上限，超限视为反应倾向不一致 → 降级候选。
/// - `max_outlier_ratio`: 孤立点比例上限，聚类超过此比例触发失败模式检查（下调 θ_nb）。
/// - `pending_expire_days`: 待定池样本超过此天数未成簇 → 低置信标记（不参与规则生成）。
/// - `evidence_decay_threshold`: 规则证据衰减后的保留率下限，低于此值 → 规则降级/失效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// 行为层总开关（false = 不学习/不路由，行为回退 v1.4）
    pub enabled: bool,
    /// 密度聚类邻域相似度阈值 θ_nb：样本对相似度 ≥ 该值视为邻居。
    /// 默认 0.65（实证可使 59 事件细分出 ≥2 个行为簇）。
    pub theta_nb: f64,
    /// 核心样本最小邻居数 min_cluster_size（默认 3）。
    pub min_cluster_size: usize,
    /// 增量归簇阈值 θ_join（默认 0.7）。
    pub theta_join: f64,
    /// 反应通道权重 β1（默认 0.85，主导 paraphrase⊕attitude 语义）。
    pub beta1: f64,
    /// 情境通道权重 β2（默认 0.10，情境关键词参与比对）。
    pub beta2: f64,
    /// 情境路由阈值 θ_route（默认 0.6，全部低于 → 不注入）
    pub theta_route: f64,
    /// 路由评分 cos 项权重 γ（默认 0.7）
    pub gamma: f64,
    /// 路由 Top-N 合并上限（默认 3）
    pub top_n: usize,
    /// 规则文本生成的证据量门槛（默认 5）
    pub min_evidence: usize,
    /// 有效样本量门槛 n_eff（默认 5）
    pub min_n_eff: usize,
    /// 簇内 valence 标准差上限（默认 0.5，超限降级候选规则）
    pub valence_std_limit: f64,
    /// 聚类孤立点比例上限（默认 0.6，超限触发失败模式检查）
    pub max_outlier_ratio: f64,
    /// 待定池样本过期天数（默认 30，超期未成簇 → 低置信标记）
    pub pending_expire_days: u32,
    /// 规则证据衰减保留率下限（默认 0.3，低于 → 降级/失效）
    pub evidence_decay_threshold: f64,
    /// 行为层近期事件加权窗口（天）：窗口内事件 recency_factor=1.0，
    /// 之后指数衰减（半衰期 = 窗口）。
    pub recent_days: i64,
}

impl Default for BehaviorConfig {
    /// 创建默认行为层配置。
    fn default() -> Self {
        Self {
            enabled: true,
            theta_nb: 0.65,
            min_cluster_size: 3,
            theta_join: 0.7,
            beta1: 0.85,
            beta2: 0.10,
            theta_route: 0.6,
            gamma: 0.7,
            top_n: 3,
            min_evidence: 5,
            min_n_eff: 5,
            valence_std_limit: 0.5,
            max_outlier_ratio: 0.6,
            pending_expire_days: 30,
            evidence_decay_threshold: 0.3,
            recent_days: 30,
        }
    }
}

// =========================================================
// 知识层配置
// =========================================================

/// 知识层配置组（`[knowledge]`）。
///
/// 职责:
/// - 控制知识层（persona_facts 生命周期 + 事实卡片注入）的开关与阈值。
/// - 总开关关闭时知识层全链路禁用，prompt 不含知识块。
///
/// 三路检索独立参数说明:
/// - 知识 fact 路的候选检索条数与路由阈值在本组独立配置
///   （`retrieve_top_k` / `retrieve_threshold`），不再依赖其他路的检索参数。
/// - 默认值 `0` / `0.0` 表示"与上一版本行为等价"（不做条数截断、沿用既有
///   判定口径）；运行时独立接线后，显式配置值才实际生效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    /// 知识层总开关（默认 false —— 自动抽取默认关闭，需用户显式开启）。
    ///
    /// `false` → 不抽取、不检索注入，prompt 不含知识块。
    pub auto_fact_detect: bool,
    /// 规则判定器开关（零新增 LLM 调用；false = 不检索注入，仅保留知识库写入能力）。
    pub detector_enabled: bool,
    /// 判重语义余弦阈值（默认 0.85）。
    pub dedup_cosine_threshold: f64,
    /// 判重关键词交集阈值（≥1 个共同词判重复）。
    pub dedup_keyword_min: u32,
    /// 多事件互证语义余弦阈值（默认 0.7）。
    pub corroboration_cosine_threshold: f64,
    /// 事实卡片注入预算（字符上限；默认 800）。
    pub injection_budget_chars: usize,
    /// volatile 事实时效半衰期（天）。
    pub volatile_halflife_days: u32,
    /// 知识 fact 路候选事实检索条数上限（本路独立参数）。
    ///
    /// `0` = 与上一版本行为等价：不按条数截断候选，仅按
    /// `injection_budget_chars` 预算注入。大于 `0` 时按条数截断。
    pub retrieve_top_k: u32,
    /// 知识 fact 路检索路由阈值（θ_route，候选命中下限，本路独立参数）。
    ///
    /// `0.0` = 与上一版本行为等价：沿用既有关键词判定口径。
    /// 大于 `0.0` 时为显式语义/关键词命中阈值。
    pub retrieve_threshold: f64,
}

impl Default for KnowledgeConfig {
    /// 创建默认知识层配置。
    ///
    /// 返回:
    /// - 自动抽取默认关闭（`auto_fact_detect=false`，需用户显式开启）。
    /// - 判重 0.85 / 互证 0.7。
    /// - 注入预算 800 字符；volatile 半衰期 30 天。
    /// - 知识路独立检索参数默认 `0` / `0.0`（行为等价占位，不在阶段一定稿）。
    fn default() -> Self {
        Self {
            auto_fact_detect: false,
            detector_enabled: true,
            dedup_cosine_threshold: 0.85,
            dedup_keyword_min: 1,
            corroboration_cosine_threshold: 0.7,
            injection_budget_chars: 800,
            volatile_halflife_days: 30,
            retrieve_top_k: 0,
            retrieve_threshold: 0.0,
        }
    }
}

// =========================================================
// 风格统计配置（表达层 A3）
// =========================================================

/// 风格统计配置（`[style]`，表达层层次 2 自动学习）。
///
/// 职责:
/// - 控制表达层风格统计（五维指标 + 显著性检验 + 自动规则生成）的开关与阈值。
/// - `enabled=false` 时整链路关闭，prompt 注入回退 v1.6（无自动风格规则，
///   回归红线 1 锁定）。
/// - `auto_translate` 仅控制"LLM 离线翻译增强"是否启用；关闭或 LLM 不可用时
///   仅使用确定性模板拼接（D-V17-002 模板优先）。
/// - `sample_fallback`：样本不足时不生成自动规则，但可写 SpeakingStyle
///   原文样例事实供画像/展示（独立开关，关闭回退纯标注）。
/// - `keyword_dict`：关键词体系衔接——存在 canonical 词表时风格候选
///   走词典增强（整词优先），关闭或词表为空回退纯 bigram。
///
/// 阈值说明（v3.1 §7.2 / D-V17-003）:
/// - `min_sample_count=200`：样本量低于此值时标注"数据不足"，不生成规则文本。
/// - 显著性判定：`|z| ≥ z_critical` 且 `频次 ≥ min_frequency` 且 `n_p ≥ min_sample_count`；
///   口癖词另加"相对超频比 > relative_boost_ratio"。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[style]` 表只写部分键时
///   缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleConfig {
    /// 风格统计总开关（默认 true —— 自动为主可配置）。
    /// `false` → 不统计、不生成规则、不注入，prompt 与 v1.6 语义等价。
    pub enabled: bool,
    /// LLM 离线翻译增强开关（默认 true —— 增强为可选，LLM 不可用静默降级模板）。
    /// `false` → 仅模板拼接（确定性可测、零 LLM 依赖）。
    pub auto_translate: bool,
    /// 小样本原文样例兜底开关（默认 true —— 样本不足时不生成自动规则，
    /// 但仍写入 SpeakingStyle 原文样例事实供画像/展示；`false` 纯标注不写样例）。
    pub sample_fallback: bool,
    /// 关键词体系衔接开关（默认 true —— 存在 keyword_pool canonical 词表时风格候选
    /// 走词典增强；`false` 或词表为空 → 回退纯 bigram）。
    pub keyword_dict: bool,
    /// 样本量阈值 n_p（默认 200 条消息）。
    /// 低于此值时标注"数据不足"，不生成规则文本、不注入。
    pub min_sample_count: u32,
    /// 口癖词/话题词 Top-N（默认 10，文档范围 10~20）。
    pub top_n: u32,
    /// 口癖词相对超频比阈值（默认 2.0：persona 频率 / 全局频率 > 2）。
    pub relative_boost_ratio: f64,
    /// 显著项最小频次（默认 5 次：频次 ≥ 5 才参与显著性判定）。
    pub min_frequency: u32,
    /// z 临界值（默认 2.0：`|z| ≥ 2` 判定统计显著）。
    pub z_critical: f64,
}

impl Default for StyleConfig {
    /// 创建默认风格统计配置。
    ///
    /// 返回:
    /// - 默认开启全链路（自动为主可配置），`auto_translate=true`（LLM 增强可选）。
    /// - 显著性判定阈值：|z|≥2 且频次≥5 且 n_p≥200；口癖词相对超频比>2。
    fn default() -> Self {
        Self {
            enabled: true,
            auto_translate: true,
            sample_fallback: true,
            keyword_dict: true,
            min_sample_count: 200,
            top_n: 10,
            relative_boost_ratio: 2.0,
            min_frequency: 5,
            z_critical: 2.0,
        }
    }
}

// =========================================================
// 弱反馈环配置（H2，自我修正闭环）
// =========================================================

/// 弱反馈环配置（`[feedback]`，S2/S3 自我修正闭环 H2）。
///
/// 职责:
/// - 控制 S2 纠正 / S3 继续发言弱信号的采集与校准行为。
/// - `auto_apply_weak_feedback` 默认关闭：弱信号只写入 `feedback_log`
///   （审计），不自动修改任何规则/画像（回归红线 5）。
/// - 检测窗口（`correction_window_ms` / `continue_window_ms`）：用户消息与
///   上一条助手回复间隔在此窗口内才判为弱信号；超时（间隔更大）不累积。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[feedback]` 表只写部分键时
///   缺失字段回退 `Default` 实现，避免解析失败。
/// - 关闭开关不破坏主流程：检测/写入失败均静默降级，不影响对话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackConfig {
    /// S2/S3 弱反馈环总开关（默认 true —— 自动采集可配置）。
    /// `false` → 不检测、不写 feedback_log，行为回退 v1.6。
    pub enabled: bool,
    /// 弱反馈是否自动应用（默认 false）。
    /// `false` → 弱信号仅写 feedback_log（审计），不触发候选复审/趋势统计的落库，
    ///          规则与画像零自动修改（回归红线 5）。
    /// `true` → 检测到 S2 纠正/ S3 趋势异常时标记候选复审（不自动覆盖规则本身）。
    pub auto_apply_weak_feedback: bool,
    /// S2 纠正前缀检测窗口（毫秒，默认 60000 = 60s）。
    /// 用户消息与上一条助手回复间隔 ≤ 此值且命中纠正前缀 → S2 纠正信号。
    pub correction_window_ms: u64,
    /// S3 继续发言检测窗口（毫秒，默认 60000 = 60s）。
    /// 用户消息与上一条助手回复间隔 ≤ 此值且非纠正 → S3 继续信号。
    pub continue_window_ms: u64,
    /// 同一目标重复反馈的去重窗口（毫秒，默认 30000 = 30s）。
    /// 窗口内同一 persona+信号+目标不重复写入（避免短时连续消息累积重复反馈）。
    pub dedup_window_ms: u64,
    /// S3 趋势统计滑动窗口大小（默认 20 次）。
    /// 取最近 N 个回合的继续/不继续结果做趋势判定。
    pub s3_trend_window: u32,
    /// S3 标记复审所需连续"继续"命中数（默认 5）。
    pub s3_continue_trigger: u32,
    /// S3 标记复审所需随后的连续"不继续"数（默认 4）。
    pub s3_stop_trigger: u32,
}

impl Default for FeedbackConfig {
    /// 创建默认弱反馈环配置。
    ///
    /// 返回:
    /// - 默认开启采集（自动为主可配置），`auto_apply_weak_feedback=false`（保守）。
    /// - 检测窗口 60s（S2/S3），去重窗口 30s。
    /// - S3 趋势窗口 20 次，连续 ≥5 次继续后 4 次不继续 → 标记复审。
    fn default() -> Self {
        Self {
            enabled: true,
            auto_apply_weak_feedback: false,
            correction_window_ms: 60_000,
            continue_window_ms: 60_000,
            dedup_window_ms: 30_000,
            s3_trend_window: 20,
            s3_continue_trigger: 5,
            s3_stop_trigger: 4,
        }
    }
}
