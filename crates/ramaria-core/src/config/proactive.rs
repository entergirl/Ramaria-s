//! crates/ramaria-core/src/config/proactive.rs - Ramaria 主动对话配置模块
//!
//! 设计特点:
//! - 定义主动对话调度的总开关、节拍与全部打扰控制参数
//! - 默认值取保守基线：低频、低打扰、总开关默认开启
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
/// - `probability`: 前置条件全部满足时的触发概率（0.0~1.0）。
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
    /// 距上次对话的最小空闲时长（小时，默认 24）。
    pub min_idle_hours: u32,
    /// 每画像每日主动投递上限（默认 1）。
    pub daily_limit: u32,
    /// 免打扰时段（`HH:MM-HH:MM`，支持跨零点；默认 "22:00-08:00"）。
    pub quiet_hours: String,
    /// 两次主动投递的最短间隔（小时，默认 48）。
    pub cooldown_hours: u32,
    /// 前置条件满足时的触发概率（0.0~1.0，默认 0.3）。
    pub probability: f64,
    /// 连续未回应退避阈值（天，默认 3）。
    pub silence_backoff_days: u32,
    /// 首次启用宽限期（天，默认 3）。
    pub startup_grace_days: u32,
}

impl Default for ProactiveConfig {
    /// 创建默认主动对话配置。
    ///
    /// 返回:
    /// - 保守基线：总开关开启，检查间隔 300 秒，最小空闲 24 小时，
    ///   每日上限 1 条，免打扰 22:00-08:00，冷却 48 小时，触发概率 0.3，
    ///   连续沉默退避 3 天，首次启用宽限 3 天。
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_seconds: 300,
            min_idle_hours: 24,
            daily_limit: 1,
            quiet_hours: "22:00-08:00".to_string(),
            cooldown_hours: 48,
            probability: 0.3,
            silence_backoff_days: 3,
            startup_grace_days: 3,
        }
    }
}
