//! crates/ramaria-memory/src/event/extractor/trigger.rs - L1→L2 事件提取触发条件判断
//!
//! 设计特点:
//! - 双条件触发：未吸收 L1 数量达标（条件 A）或最早未吸收 L1 超期（条件 B）
//! - 满足任一条件即触发提取
//! - 读取失败转为 `RamariaError::storage` 上抛，保留上下文
//! - 仅被父模块 `extractor` 调用，方法以 `pub(super)` 对外可见

use ramaria_core::types::now_ms;
use ramaria_core::{RamariaError, RamariaResult};
use tracing::{debug, warn};

use super::{EventExtractor, MS_PER_DAY};

impl<'a> EventExtractor<'a> {
    /// 检查是否满足事件提取触发条件。
    ///
    /// 条件 A: 未吸收 L1 ≥ trigger_count
    /// 条件 B: 最早未吸收 L1 距今 ≥ trigger_days 天
    ///
    /// 满足任一条件即触发。
    pub(super) async fn should_trigger(&self, persona_uid: &str) -> RamariaResult<bool> {
        let l1_list = match self.storage.list_unabsorbed_l1(persona_uid).await {
            Ok(list) => list,
            Err(e) => {
                warn!(%persona_uid, error=%e, "检查触发条件时读取 L1 失败");
                return Err(RamariaError::storage(format!(
                    "检查 {persona_uid} 触发条件失败: {e}"
                )));
            }
        };

        // 条件 A: 数量
        if l1_list.len() >= self.config.trigger_count as usize {
            debug!(
                %persona_uid,
                count = l1_list.len(),
                threshold = self.config.trigger_count,
                "满足触发条件 A: 数量 ≥ {}",
                self.config.trigger_count
            );
            return Ok(true);
        }

        // 条件 B: 时间
        if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
            let now = now_ms();
            let age_ms = now.saturating_sub(oldest);
            // 使用 f64 避免整数除法截断: 23h59m 不应算作 1 天
            let age_days = (age_ms as f64) / MS_PER_DAY;
            if age_days >= self.config.trigger_days as f64 {
                debug!(
                    %persona_uid,
                    age_days,
                    threshold = self.config.trigger_days,
                    "满足触发条件 B: 最早 L1 距今 {} 天 ≥ {} 天",
                    age_days,
                    self.config.trigger_days
                );
                return Ok(true);
            }
        }

        Ok(false)
    }
}
