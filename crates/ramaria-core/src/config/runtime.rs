//! crates/ramaria-core/src/config/runtime.rs - Ramaria 运行期配置模块
//!
//! 设计特点:
//! - 定义 Session 生命周期与空闲阈值配置
//! - 定义聚类等阈值配置
//! - 定义事件提取（L1→L2）配置及 serde 缺省回退
//! - 定义记忆检索索引配置
//! - 支持 serde，各配置组提供稳定默认值

use serde::{Deserialize, Serialize};

// =========================================================
// Session 管理配置
// =========================================================

/// Session 生命周期管理参数。
///
/// 职责:
/// - 描述空闲多久触发 L1 摘要。
/// - 描述后台检查间隔和对话历史保留规模。
///
/// 说明:
/// - 优先沿用现有 Python session 行为。
/// - 上层 app 编排层负责解释这些参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// 空闲超过此时长（分钟）自动触发 L1 摘要
    pub l1_idle_minutes: u32,
    /// 空闲检测轮询间隔（秒）
    pub idle_check_interval_seconds: u32,
    /// L2 定时检查间隔（秒）
    pub l2_check_interval_seconds: u32,
    /// 对话历史加载条数上限（进入上下文的历史窗口；分页倒序加载）
    pub max_history_messages: u32,
    /// 会话历史字符预算上限（消息内容 + role 标记的粗略字符数；超限保留最近内容）
    pub max_history_chars: u32,
}

impl Default for SessionConfig {
    /// 创建默认 Session 管理参数。
    ///
    /// 返回:
    /// - 10 分钟空闲触发 L1。
    /// - 最多加载 200 条对话历史供上下文使用。
    /// - 历史窗口字符预算 6000。
    fn default() -> Self {
        Self {
            l1_idle_minutes: 10,
            idle_check_interval_seconds: 60,
            l2_check_interval_seconds: 86400,
            max_history_messages: 200,
            max_history_chars: 6000,
        }
    }
}

// =========================================================
// 记忆层触发阈值
// =========================================================

/// 记忆层触发阈值。
///
/// 职责:
/// - 控制何时将未吸收 L1 合并为 L2（路径 A 计数触发 + 路径 B 时间触发）。
/// - 控制何时触发 L3 性格推断（路径 A 计数触发 + 路径 B 时间触发）。
/// - 对齐 Python `MergerConfig` + `ProfileConfig` 的触发策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThresholdConfig {
    /// 未吸收 L1 触发 L2 合并的条数阈值（路径 A）
    pub l2_trigger_count: u32,
    /// 最早未吸收 L1 触发 L2 的天数阈值（路径 B）
    pub l2_trigger_days: u32,
    /// 未吸收事件触发 L3 推断的条数阈值（路径 A）
    pub l3_trigger_count: u32,
    /// 最早未吸收事件触发 L3 推断的天数阈值（路径 B）
    pub l3_trigger_days: u32,
    /// L2 事件提取时簇间 LLM 请求间隔（毫秒），用于避免触发远程 API 速率限制。
    /// 默认 800（等待 800ms）；建议对 DeepSeek 等有速率限制的 API 调大。
    /// 显式配置 `0` 合法（表示不等待），仅在键缺失时回退默认值。
    #[serde(default = "default_cluster_delay_ms")]
    pub cluster_delay_ms: u64,
}

/// serde 默认值：簇间 LLM 请求间隔 800ms（与 `Default` 实现保持一致）。
fn default_cluster_delay_ms() -> u64 {
    800
}

impl Default for ThresholdConfig {
    /// 创建默认记忆层触发阈值。
    ///
    /// 返回:
    /// - 5 条未吸收 L1 或最早未吸收 L1 超过 7 天时触发 L2 检查。
    /// - 10 条未吸收事件或最早事件超过 30 天时触发 L3 推断。
    fn default() -> Self {
        Self {
            l2_trigger_count: 5,
            l2_trigger_days: 7,
            l3_trigger_count: 10,
            l3_trigger_days: 30,
            cluster_delay_ms: 800,
        }
    }
}

// =========================================================
// 事件提取配置（L1→L2）
// =========================================================

/// 事件提取器 LLM 参数。
///
/// 职责:
/// - 控制 EventExtractor 调用 LLM 时的 `max_tokens`、`temperature` 和单簇最大事件数。
/// - 独立于全局 `[backend]` 配置，因为事件提取的 JSON 输出需要比对话大得多的 token 预算。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventExtractionConfig {
    /// LLM 生成温度（0.0-2.0）
    #[serde(default = "default_event_extraction_temperature")]
    pub temperature: f64,
    /// 最大输出 token 数（事件 JSON 较长，需比对话大）
    #[serde(default = "default_event_extraction_max_tokens")]
    pub max_tokens: u32,
    /// 单簇最多提取的事件数
    #[serde(default = "default_event_extraction_max_events")]
    pub max_events: usize,
    /// 降级事件动态置信度公式开关。
    /// `true` → `min(0.59, 0.35 + 0.02 × n_l1)` 封顶 0.59 恒 tentative；
    /// `false` → 回退固定 `default_confidence`（0.5）。
    #[serde(default = "default_degraded_confidence_enabled")]
    pub degraded_confidence_enabled: bool,
}

fn default_event_extraction_temperature() -> f64 {
    0.3
}
fn default_event_extraction_max_tokens() -> u32 {
    8192
}
fn default_event_extraction_max_events() -> usize {
    5
}
fn default_degraded_confidence_enabled() -> bool {
    true
}

impl Default for EventExtractionConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 8192,
            max_events: 5,
            degraded_confidence_enabled: true,
        }
    }
}

// =========================================================
// 索引配置
// =========================================================

/// 索引相关参数。
///
/// 职责:
/// - 控制 BM25 增量更新和周期性重建节奏。
/// - 控制内存索引跨进程刷新（代次比对触发重建）的重建节流。
/// - 为后续向量索引和图谱索引配置预留扩展位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// BM25 增量合并阈值（缓冲区积累超过此条数触发合并）
    pub bm25_incremental_threshold: u32,
    /// BM25 定时重建间隔（秒）
    pub bm25_rebuild_interval: u32,
    /// 内存索引两次重建之间的最小间隔（秒）。
    ///
    /// 语义:
    /// - `0`（默认）: 不节流——每次召回按代次比对，跨进程写入即时可见；
    /// - 大于 `0`: 冷却窗口内检测到代次变化也不重建（沿用现有索引），
    ///   窗口过后的下一次召回补上；用于写入密集期抑制整库重建风暴，
    ///   代价是跨进程写入的可见延迟不超过该间隔。
    pub refresh_interval_seconds: u32,
}

impl Default for IndexConfig {
    /// 创建默认索引参数。
    ///
    /// 返回:
    /// - BM25 缓冲区积累 10 条后合并。
    /// - 每 300 秒进行一次重建检查。
    /// - 内存索引重建不做节流（跨进程写入即时可见）。
    fn default() -> Self {
        Self {
            bm25_incremental_threshold: 10,
            bm25_rebuild_interval: 300,
            refresh_interval_seconds: 0,
        }
    }
}
