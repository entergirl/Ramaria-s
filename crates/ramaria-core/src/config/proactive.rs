//! crates/ramaria-core/src/config/proactive.rs - Ramaria 主动对话配置模块
//!
//! 设计特点:
//! - 定义主动对话调度的总开关、节拍与全部打扰控制参数
//! - 默认值取均衡基线：AI 判据与软加权触发、打扰可控、总开关默认开启
//! - 各字段提供 serde 缺省回退，配置文件只写部分键时缺失字段回退默认值
//! - 只描述数据，不负责调度实现（调度在服务层生命周期循环）
//! - 与行为层 `BehaviorParams::proactiveness` 无语义关联（命名口径显式区分）

use serde::{Deserialize, Serialize};

// =========================================================
// 主动对话配置
// =========================================================

/// 主动对话配置（`[proactive]` 配置组）。
///
/// 职责:
/// - 控制"系统主动发起对话"能力的总开关、调度节拍与打扰控制参数。
/// - 由桌面宿主装配的调度循环读取；MCP / CLI 不装配该能力（不代客户端发言）。
///
/// 字段约定:
/// - `enabled`: 总开关（默认开启）；关闭后不调度、不投递。
/// - `check_interval_seconds`: 调度循环的检查间隔（秒），决定触发判定频率。
/// - `min_idle_hours`: 距上次对话的最小空闲时长（小时），避免打断进行中的交流。
/// - `daily_limit`: 每个画像每日最多主动投递条数。
/// - `quiet_hours`: 免打扰时段（`HH:MM-HH:MM`，支持跨零点，如 `"22:00-08:00"`）。
/// - `cooldown_hours`: 两次主动投递之间的最短间隔（小时）。
/// - `judge_enabled`: 判据开关（默认开启）；关闭后由算法打分直接决策。
/// - `judge_interval_hours`: 判据调用节流间隔（小时），两次判据调用之间的最短间隔。
/// - `active_hours_weight`: 活跃时段软加权强度（0.0~1.0，越高越偏向用户活跃时段）。
/// - `active_hours_window_days`: 活跃时段统计滚动窗口（天）。
/// - `active_hours_min_samples`: 时段建模最少样本数（不足时不启用时段加权）。
/// - `valence_weight`: 效价入权强度（越高情绪波动大的记忆越优先）。
/// - `confidence_floor`: 事件置信度门槛（低于此值不进入主动选题，0.0~1.0）。
/// - `light_touch_weight`: 轻触达兜底候选权重（0.0~1.0）。
/// - `silence_backoff_days`: 连续未回应达到此天数后进入退避（降频 / 暂停）。
/// - `startup_grace_days`: 首次启用后的宽限期（天），期内不触发。
///
/// 命名说明:
/// - 本组与行为层 `crate::behavior::BehaviorParams::proactiveness` 无语义关联：
///   后者是行为规则的结构化参数（措辞主动程度的生成微调），不参与调度与投递控制。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 的 `[proactive]` 表只写部分键时，
///   缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProactiveConfig {
    /// 主动对话总开关（默认 true）。
    pub enabled: bool,
    /// 调度检查间隔（秒，默认 300）。
    pub check_interval_seconds: u32,
    /// 距上次对话的最小空闲时长（小时，默认 4）。
    pub min_idle_hours: u32,
    /// 每画像每日主动投递上限（默认 3）。
    pub daily_limit: u32,
    /// 免打扰时段（`HH:MM-HH:MM`，支持跨零点；默认 "22:00-08:00"）。
    pub quiet_hours: String,
    /// 两次主动投递的最短间隔（小时，默认 8）。
    pub cooldown_hours: u32,
    /// 判据开关（默认 true）；关闭后由算法打分直接决策。
    pub judge_enabled: bool,
    /// 判据调用节流间隔（小时，默认 3）。
    pub judge_interval_hours: u32,
    /// 活跃时段软加权强度（0.0~1.0，默认 0.8；越高越偏向用户活跃时段）。
    pub active_hours_weight: f64,
    /// 活跃时段统计滚动窗口（天，默认 30）。
    pub active_hours_window_days: u32,
    /// 时段建模最少样本数（默认 50；不足时不启用时段加权）。
    pub active_hours_min_samples: u32,
    /// 效价入权强度（默认 0.5；越高情绪波动大的记忆越优先）。
    pub valence_weight: f64,
    /// 事件置信度门槛（0.0~1.0，默认 0.6；低于此值不进入主动选题）。
    pub confidence_floor: f64,
    /// 轻触达兜底候选权重（0.0~1.0，默认 0.3）。
    pub light_touch_weight: f64,
    /// 连续未回应退避阈值（天，默认 3）。
    pub silence_backoff_days: u32,
    /// 首次启用宽限期（天，默认 3）。
    pub startup_grace_days: u32,
}

impl Default for ProactiveConfig {
    /// 创建默认主动对话配置。
    ///
    /// 返回:
    /// - 均衡基线：总开关开启，检查间隔 300 秒，最小空闲 4 小时，
    ///   每日上限 3 条，免打扰 22:00-08:00，冷却 8 小时；判据开启、
    ///   节流 3 小时；时段加权 0.8（窗口 30 天、最少 50 样本）；
    ///   效价权重 0.5，置信度门槛 0.6，轻触达权重 0.3；
    ///   连续沉默退避 3 天，首次启用宽限 3 天。
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_seconds: 300,
            min_idle_hours: 4,
            daily_limit: 3,
            quiet_hours: "22:00-08:00".to_string(),
            cooldown_hours: 8,
            judge_enabled: true,
            judge_interval_hours: 3,
            active_hours_weight: 0.8,
            active_hours_window_days: 30,
            active_hours_min_samples: 50,
            valence_weight: 0.5,
            confidence_floor: 0.6,
            light_touch_weight: 0.3,
            silence_backoff_days: 3,
            startup_grace_days: 3,
        }
    }
}
