//! crates/ramaria-service/src/config/single_key.rs - Ramaria 单键配置写入用例
//!
//! 设计特点:
//! - 读当前生效配置（config.toml 与 DB 合并结果）→ 只改目标键 → 走统一写入口双写
//! - 未涉及的配置组按读取结果原样写回，避免整份配置回写造成字段丢失
//! - 写入结果原样回传双侧状态与失败明细，由入口层决定提示方式
//! - 双侧全部成功时内存快照随统一写入口热替换，运行期立即生效

use ramaria_core::error::RamariaResult;

use crate::engine::Engine;

// =========================================================
// 结果类型
// =========================================================

/// 单键写入结果（文件侧 + DB 侧完成情况）。
///
/// 字段约定:
/// - `file_ok`: config.toml 是否写入成功；
/// - `db_ok`: settings / backend_config 表是否全部写入成功；
/// - `failures`: 未成功步骤的失败原因（双侧均成功时为空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleKeyWriteOutcome {
    pub file_ok: bool,
    pub db_ok: bool,
    pub failures: Vec<String>,
}

// =========================================================
// 图片理解声明
// =========================================================

/// 写入图片理解声明（`[vision].model_supports_vision`）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `enabled`: 当前对话模型是否支持图片识别。
///
/// 返回:
/// - 双侧写入结果（单侧失败降级不阻塞，明细见 `failures`）；
/// - 读取或写入失败时返回对应错误。
///
/// 说明:
/// - 只修改一个键，`[vision].batch_limit` 与其它配置组按读取值保留；
/// - 热替换范围仅配置快照，索引与后台阈值不受影响。
pub async fn set_vision_support(
    engine: &Engine,
    enabled: bool,
) -> RamariaResult<SingleKeyWriteOutcome> {
    let mut config = engine.load_full_config().await?;
    config.vision.model_supports_vision = enabled;

    let result = engine.save_config(&config).await?;
    tracing::info!(
        enabled,
        file_ok = result.file_ok,
        db_ok = result.db_ok,
        failures = result.failures.len(),
        "图片理解声明已写入"
    );

    Ok(SingleKeyWriteOutcome {
        file_ok: result.file_ok,
        db_ok: result.db_ok,
        failures: result.failures,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
