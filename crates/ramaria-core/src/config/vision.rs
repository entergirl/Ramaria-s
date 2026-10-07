//! crates/ramaria-core/src/config/vision.rs - Ramaria 图片理解配置模块
//!
//! 设计特点:
//! - 定义图片附件理解的能力声明与单批处理上限
//! - 默认关闭：显式声明当前模型支持图片识别后才执行理解
//! - 各字段提供 serde 缺省回退，配置文件只写部分键时缺失字段回退默认值
//! - 只描述数据，不负责理解任务实现（任务编排在服务层）

use serde::{Deserialize, Serialize};

// =========================================================
// 图片理解配置
// =========================================================

/// 图片理解配置（`[vision]` 配置组）。
///
/// 职责:
/// - 控制导入 / 对话中图片附件的描述生成能力：能力声明与单批处理上限。
/// - 由服务层理解任务读取；声明关闭时图片理解整体跳过（附件保留占位符）。
///
/// 字段约定:
/// - `model_supports_vision`: 当前对话模型是否支持图片识别（显式声明）；
///   关闭时图片理解整体跳过。
/// - `batch_limit`: 单次理解批次的图片上限（0 = 不限）。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 的 `[vision]` 表只写部分键时，
///   缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionConfig {
    /// 当前对话模型是否支持图片识别（默认 false；显式声明后才执行理解）。
    pub model_supports_vision: bool,
    /// 单次理解批次的图片上限（默认 0 = 不限）。
    pub batch_limit: u32,
}

impl Default for VisionConfig {
    /// 创建默认图片理解配置。
    ///
    /// 返回:
    /// - 能力声明关闭（图片理解整体跳过）、单批上限 0（不限）。
    fn default() -> Self {
        Self {
            model_supports_vision: false,
            batch_limit: 0,
        }
    }
}
