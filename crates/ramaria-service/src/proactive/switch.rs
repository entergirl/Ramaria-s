//! crates/ramaria-service/src/proactive/switch.rs - Ramaria 主动对话人格开关模块
//!
//! 设计特点:
//! - 三态开关按画像存于 `settings` 表，键 `proactive.persona.{persona_uid}`
//! - 自动 / 手动开 / 手动关：手动态覆盖自动判定，缺失与非法值回退自动
//! - 三态均显式存储（无删键路径）：读取方只认三值，不依赖键存在性
//! - 日志只记键名与状态值，不记消息内容

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;

// =========================================================
// 三态定义
// =========================================================

/// 人格主动开关三态。
///
/// 职责:
/// - 描述单个人格参与主动对话的资格：自动（按对话历史判定）/ 手动开 / 手动关。
///
/// 字段约定:
/// - `Auto`: 未显式配置或值非法时的回退态：无对话历史视为冷启动不参与；
/// - `On`: 手动强开：无对话历史也放行（其余打扰控制照常）；
/// - `Off`: 手动关闭：一律不参与主动调度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProactivePersonaMode {
    Auto,
    On,
    Off,
}

impl ProactivePersonaMode {
    /// 稳定字符串标识（存储文本）。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProactivePersonaMode::Auto => "auto",
            ProactivePersonaMode::On => "on",
            ProactivePersonaMode::Off => "off",
        }
    }

    /// 解析存储文本（`trim` 后精确匹配三值，其余返回 None）。
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "auto" => Some(ProactivePersonaMode::Auto),
            "on" => Some(ProactivePersonaMode::On),
            "off" => Some(ProactivePersonaMode::Off),
            _ => None,
        }
    }
}

// =========================================================
// 读写封装
// =========================================================

/// 开关键前缀（完整键 = `proactive.persona.{persona_uid}`）。
const SWITCH_KEY_PREFIX: &str = "proactive.persona.";

/// 组装指定画像的开关键。
fn switch_key(persona_uid: &str) -> String {
    format!("{SWITCH_KEY_PREFIX}{persona_uid}")
}

/// 读取指定画像的主动开关状态。
///
/// 参数:
/// - `storage`: 存储后端（`settings` 表键值读写）。
/// - `persona_uid`: 画像标识。
///
/// 返回:
/// - 键缺失 → 自动（尚未显式设置过开关）；
/// - 值非法 → 记 warn 并回退自动（开关可重设，不阻塞调度）；
/// - 存储读取本身失败 → 返回 `Storage` 错误（不静默吞错）。
pub(crate) async fn load_mode(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<ProactivePersonaMode> {
    let key = switch_key(persona_uid);
    let Some(raw) = storage.get_setting(&key).await? else {
        return Ok(ProactivePersonaMode::Auto);
    };

    match ProactivePersonaMode::parse(&raw) {
        Some(mode) => Ok(mode),
        None => {
            tracing::warn!(key = %key, "主动开关值非法，回退自动");
            Ok(ProactivePersonaMode::Auto)
        }
    }
}

/// 写入指定画像的主动开关状态（已存在键覆盖写；三态均显式存储）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona_uid`: 画像标识。
/// - `mode`: 待保存的开关状态。
///
/// 返回:
/// - 写入失败 → `Storage` 错误。
///
/// 消费方:
/// - 命令面写入口与测试；调度读取路径只走 [`load_mode`]。
#[allow(dead_code)]
pub(crate) async fn save_mode(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    mode: ProactivePersonaMode,
) -> RamariaResult<()> {
    let key = switch_key(persona_uid);
    storage.set_setting(&key, mode.as_str()).await?;

    tracing::debug!(key = %key, mode = mode.as_str(), "主动开关已保存");
    Ok(())
}

#[cfg(test)]
mod tests;
