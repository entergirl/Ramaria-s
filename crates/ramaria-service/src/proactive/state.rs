//! crates/ramaria-service/src/proactive/state.rs - Ramaria 主动对话运行时状态模块
//!
//! 设计特点:
//! - 状态以 JSON 存于 `settings` 表，键 `proactive.state.{persona_uid}`，重启保持
//! - 缺键 / 损坏 / 类型不符的 JSON 回退默认状态并记 warn，不阻塞调度
//! - 状态形态覆盖打扰控制与去重冷却的全部跨 tick 数据
//! - 日志只记状态键名与解析错误，不记状态值内容
//! - 按画像隔离：同一画像读写同一键，不同画像互不串扰

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use serde::{Deserialize, Serialize};

// =========================================================
// 状态形态
// =========================================================

/// 主动对话运行时状态（按画像持久化）。
///
/// 职责:
/// - 承载打扰控制与去重冷却所需的跨 tick 数据：上次投递时间、当日计数与归属
///   日期、连续未回应次数、近期选题记录。
///
/// 字段约定:
/// - `last_sent_at`: 上次成功投递时间（Unix 毫秒；None = 尚未投递过）。
/// - `daily_count` + `daily_date`: 当日已投递条数与计数归属日期
///   （本地日期 `YYYY-MM-DD` 文本）；跨日重置由调用方按日期比对执行。
/// - `silence_streak`: 主动消息后连续未得到用户回应的累计次数（退避依据）。
/// - `recent_topics`: 近期选题记录（同一事件 / 规则的去重冷却依据）。
///
/// 兼容性:
/// - 字段级 `#[serde(default)]`：状态 JSON 缺字段（版本演进）时回退字段默认值。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProactiveState {
    #[serde(default)]
    pub last_sent_at: Option<i64>,
    #[serde(default)]
    pub daily_count: u32,
    #[serde(default)]
    pub daily_date: String,
    #[serde(default)]
    pub silence_streak: u32,
    #[serde(default)]
    pub recent_topics: Vec<RecentTopic>,
}

/// 近期选题记录（去重冷却输入）。
///
/// 字段约定:
/// - `source`: 选题来源标识（与选题器的来源标识一致，如 `event` / `rule`）。
/// - `key`: 来源内的稳定标识（事件 id / 规则 id 的文本形态）。
/// - `sent_at`: 该选题的投递时间（Unix 毫秒），冷却窗口以此为起点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentTopic {
    pub source: String,
    pub key: String,
    pub sent_at: i64,
}

// =========================================================
// 读写封装
// =========================================================

/// 状态键前缀（完整键 = `proactive.state.{persona_uid}`）。
const STATE_KEY_PREFIX: &str = "proactive.state.";

/// 组装指定画像的状态键。
fn state_key(persona_uid: &str) -> String {
    format!("{STATE_KEY_PREFIX}{persona_uid}")
}

/// 读取指定画像的主动对话状态。
///
/// 参数:
/// - `storage`: 存储后端（`settings` 表键值读写）。
/// - `persona_uid`: 画像标识。
///
/// 返回:
/// - 键缺失 → 默认状态（尚未产生过主动行为）；
/// - JSON 损坏 / 字段类型不符 → 记 warn 并回退默认状态（状态可重建，不阻塞调度）；
/// - 存储读取本身失败 → 返回 `Storage` 错误（不静默吞错）。
pub(crate) async fn load_state(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<ProactiveState> {
    let key = state_key(persona_uid);
    let Some(raw) = storage.get_setting(&key).await? else {
        return Ok(ProactiveState::default());
    };

    match serde_json::from_str::<ProactiveState>(&raw) {
        Ok(state) => Ok(state),
        Err(e) => {
            // 状态为可重建的运行期数据：损坏时回退默认，避免阻塞后续调度
            tracing::warn!(
                key = %key,
                error = %e,
                "主动对话状态 JSON 解析失败，回退默认状态"
            );
            Ok(ProactiveState::default())
        }
    }
}

/// 写入指定画像的主动对话状态（已存在键覆盖写）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `persona_uid`: 画像标识。
/// - `state`: 待持久化的完整状态快照。
///
/// 返回:
/// - 序列化失败 → `Serialization` 错误；
/// - 写入失败 → `Storage` 错误。
pub(crate) async fn save_state(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    state: &ProactiveState,
) -> RamariaResult<()> {
    let key = state_key(persona_uid);
    let json = serde_json::to_string(state)
        .map_err(|e| RamariaError::serialization(format!("序列化主动对话状态失败: {e}")))?;
    storage.set_setting(&key, &json).await?;

    tracing::debug!(key = %key, "主动对话状态已保存");
    Ok(())
}

#[cfg(test)]
mod tests;
