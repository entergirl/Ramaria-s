//! crates/ramaria-memory/src/event/extractor/batch.rs - 降级事件与提取指纹登记
//!
//! 设计特点:
//! - `degrade_cluster`: 单簇降级计算（纯计算，不写库），由调用方累积进批次
//! - `record_fingerprint_if_no_output`: 无产出时登记 L1 集合指纹（v1.5 去重）
//! - 指纹登记失败仅记 warn（降级：下次重复聚类，不阻塞主流程）
//! - 仅被父模块 `extractor` 调用，方法以 `pub(super)` 对外可见

use ramaria_core::MemoryEvent;
use ramaria_core::types::MemoryL1;
use tracing::{info, warn};
use uuid::Uuid;

use crate::event::degrade::build_degraded_event;

use super::EventExtractor;

impl<'a> EventExtractor<'a> {
    /// 记录"已聚类且无产出"的 L1 集合指纹（v1.5 L2 聚类去重指纹）。
    ///
    /// 语义:
    /// - 仅当指纹开关开启且 `event_count == 0`（无事件产出）时登记；
    ///   有产出时 L1 会被标记 absorbed，下次集合变化指纹自然失效，无需登记。
    /// - 登记失败仅记 warn（降级：下次会重复聚类，不阻塞主流程）。
    pub(super) async fn record_fingerprint_if_no_output(
        &self,
        persona_uid: &str,
        fingerprint: &str,
        event_count: usize,
    ) {
        if !self.config.l2_fingerprint_enabled || event_count > 0 {
            return;
        }
        match self
            .storage
            .save_l2_fingerprint(persona_uid, fingerprint)
            .await
        {
            Ok(_) => {
                info!(
                    %persona_uid,
                    fingerprint = %fingerprint,
                    "L2 无产出：已登记 L1 集合指纹（同集合下次直接跳过）"
                );
            }
            Err(e) => {
                warn!(
                    %persona_uid,
                    fingerprint = %fingerprint,
                    error = %e,
                    "L2 指纹登记失败（下次将重复聚类，不阻塞）"
                );
            }
        }
    }

    /// 单簇降级计算（纯计算，不写库）。
    ///
    /// 说明:
    /// - 返回的降级事件与来源权重由调用方累积进 `EventBatchWrite`，
    ///   随整批在单事务内写入；事件 id 由存储层在批次事务内回填。
    ///
    /// 返回:
    /// - `(降级事件, 该簇 L1 及其来源权重列表)`。
    pub(super) fn degrade_cluster(
        &self,
        persona_uid: &str,
        l1_batch: &[&MemoryL1],
    ) -> (MemoryEvent, Vec<(Uuid, f64)>) {
        let l1_owned: Vec<MemoryL1> = l1_batch.iter().map(|l| (*l).clone()).collect();
        let event = build_degraded_event(persona_uid, &l1_owned, &self.config.degrade);
        let weight = if l1_batch.is_empty() {
            0.0
        } else {
            1.0 / l1_batch.len() as f64
        };
        let sources = l1_batch.iter().map(|l1| (l1.id, weight)).collect();
        (event, sources)
    }
}
