//! crates/ramaria-service/src/lifecycle/options.rs - Ramaria 生命周期装配选项
//!
//! 设计特点:
//! - 宿主差异全部由 [`LifecycleOptions`] 表达：desktop（全开）/ mcp（仅空闲检查）/ none（不拉起循环）
//! - 间隔与延迟均为可选覆盖值：`None` 回退配置或默认值，显式值由调用方自保证
//! - 链式 `with_*` 覆盖空闲间隔、L2/L3 检查间隔与首轮延迟
//! - 缺省（`Default`）按桌面宿主口径（长驻宿主，行为最全）

/// 生命周期装配选项（宿主差异全部由此表达）。
///
/// 字段约定:
/// - `idle`: 是否拉起空闲检查循环（超时会话自动封存）；
/// - `l2_l3`: 是否拉起 L2/L3 定时调度循环；
/// - `startup_l1_retry`: 是否执行启动期一次 L1 补扫（延迟后先检查停止位再执行）；
/// - `idle_interval_seconds`: 空闲检查间隔覆盖值；`None` 取
///   `[session].idle_check_interval_seconds`（并夹取到 [`crate::idle::MIN_IDLE_CHECK_INTERVAL_SECONDS`]）；
///   显式值不做下限夹取（调用方保证不小于 1 秒，测试与特殊宿主使用）；
/// - `l2_l3_interval_seconds`: L2/L3 检查间隔覆盖值；`None` 取
///   `[session].l2_check_interval_seconds`；
/// - `l2_l3_first_delay_seconds`: L2/L3 首轮检查延迟覆盖值；`None` 取默认 300 秒。
#[derive(Debug, Clone)]
pub struct LifecycleOptions {
    pub idle: bool,
    pub l2_l3: bool,
    pub startup_l1_retry: bool,
    pub idle_interval_seconds: Option<u64>,
    pub l2_l3_interval_seconds: Option<u64>,
    pub l2_l3_first_delay_seconds: Option<u64>,
}

impl LifecycleOptions {
    /// 桌面宿主：空闲检查 + L2/L3 调度 + 启动期 L1 补扫全开（长驻宿主，行为最全）。
    pub fn desktop() -> Self {
        Self {
            idle: true,
            l2_l3: true,
            startup_l1_retry: true,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// MCP 宿主：仅空闲检查（不拉起 L2/L3 调度，也不做启动期补扫）。
    pub fn mcp() -> Self {
        Self {
            idle: true,
            l2_l3: false,
            startup_l1_retry: false,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// 单次执行宿主：不拉起任何后台循环。
    pub fn none() -> Self {
        Self {
            idle: false,
            l2_l3: false,
            startup_l1_retry: false,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// 覆盖空闲检查间隔（秒；显式值不做下限夹取）。
    pub fn with_idle_interval(mut self, seconds: u64) -> Self {
        self.idle_interval_seconds = Some(seconds);
        self
    }

    /// 覆盖 L2/L3 检查间隔（秒）。
    pub fn with_l2_l3_interval(mut self, seconds: u64) -> Self {
        self.l2_l3_interval_seconds = Some(seconds);
        self
    }

    /// 覆盖 L2/L3 首轮检查延迟（秒）。
    pub fn with_l2_l3_first_delay(mut self, seconds: u64) -> Self {
        self.l2_l3_first_delay_seconds = Some(seconds);
        self
    }
}

impl Default for LifecycleOptions {
    /// 缺省按桌面宿主口径（长驻宿主，行为最全）。
    fn default() -> Self {
        Self::desktop()
    }
}
