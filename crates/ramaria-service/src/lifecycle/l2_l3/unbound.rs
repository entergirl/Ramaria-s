//! crates/ramaria-service/src/lifecycle/l2_l3/unbound.rs - Ramaria 无主 L1 归属处理（数据断层修复）
//!
//! 设计特点:
//! - 导入产生的 L1 固定 persona_uid=NULL，按来源 session 的归属 persona 归并后进入标准 L2 提取
//! - 归属幂等：仅更新仍为 NULL 且未吸收的 L1（`assign_l1_persona_uid`），重复调用不覆盖既有归属
//! - 触发双线：计数（路径 A）与时间（路径 B）独立判定，满足任一即触发；两者均关闭时不触发
//! - 降级纪律：单组查询 / 归属失败只跳过该组并记日志，不阻塞其他组处理
//! - 隐私：日志只记 persona / session ID 与条数，不记摘要内容

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use ramaria_core::types::{MemoryL1, now_ms};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::engine::Engine;

use super::l2::{run_l2_extraction, shutdown_requested};

// =========================================================
// 无主 L1 处理（数据断层修复）：L2 触发链路补全
// =========================================================

/// 无主 L1 处理统计（供日志聚合与可观测性）。
#[derive(Debug, Default, Clone)]
pub(super) struct UnboundL1ProcessStats {
    /// 无主未吸收 L1 总数
    pub(super) total: usize,
    /// 已归属到 persona 的条数（可触发候选）
    pub(super) attributed: usize,
    /// 无法归属的条数（来源 session 缺失或 session.persona_uid 为 NULL）
    pub(super) unattributable: usize,
    /// 达到触发条件并启动 L2 提取的 persona 组数
    pub(super) triggered_personas: usize,
    /// 未达触发条件、保持无主状态待下次检查的 persona 组数
    pub(super) pending_groups: usize,
}

/// 处理"无主"L1（`persona_uid IS NULL`，导入产生的 L1 属此类）。
///
/// 背景（数据断层修复）:
/// - 导入的 L1 摘要固定 NULL 归属（摘要不应被特定画像独占），
///   但 L2 事件提取严格按 persona 遍历 `list_unabsorbed_l1(persona_uid)`，
///   NULL 归属的 L1 对任何 persona 都查不到 → L2 永不触发 → 事件恒为 0。
/// - 本函数打通该链路：把无主 L1 按来源 session 的归属 persona 归并，
///   满足触发条件（计数/时间二选一）时回填 persona_uid 后走标准 L2 提取。
///
/// 归属规则:
/// - 每条无主 L1 的 `session_id` → `sessions.persona_uid` 即其归属 persona
///   （导入场景下 session 归属为处理侧 persona，即"对方"画像）。
/// - session 不存在 / session.persona_uid 为 NULL / 查询失败 → 无法归属，
///   保持无主状态（记 warn + 统计），不阻塞其他组的处理。
///
/// 触发语义:
/// - `trigger_count > 0`：启用计数触发（路径 A），未吸收 L1 ≥ 该值即触发。
/// - `trigger_days > 0.0`：启用时间触发（路径 B），最早无主 L1 年龄 ≥ 该值即触发。
/// - 两者均 > 0 时满足任一即触发；均为 0 时不触发（安全默认）。
///
/// 幂等与降级:
/// - 归属仅更新仍为 NULL 且未吸收的 L1（`assign_l1_persona_uid` 幂等），
///   重复调用不会覆盖既有归属。
/// - 归属失败仅跳过该组（记 error），不阻塞其他组的 L2 提取。
/// - 提取成功时 L1 被标记 absorbed，后续检查自然跳过；
///   提取失败（LLM 不可用）时 L1 已归属到 persona，由标准 persona 循环负责重试。
pub(super) async fn process_unbound_l1_for_l2(
    engine: &Engine,
    shutdown: Option<&AtomicBool>,
    trigger_count: usize,
    trigger_days: f64,
) -> UnboundL1ProcessStats {
    let storage = engine.storage_ref().as_ref();
    let mut stats = UnboundL1ProcessStats::default();

    // 1. 读取无主未吸收 L1（storage 既有通道，此前仅检索索引使用）
    let unbound = match storage.list_unabsorbed_l1_unbound().await {
        Ok(list) => list,
        Err(e) => {
            error!(error = %e, "L2 无主 L1 处理：查询无主未吸收 L1 失败");
            return stats;
        }
    };
    stats.total = unbound.len();
    if unbound.is_empty() {
        return stats;
    }
    info!(
        total = unbound.len(),
        "L2 触发检查：发现无主 L1，开始归属处理（数据断层修复链路）"
    );

    // 2. 按来源 session 分组（同一 session 的 L1 归属相同，去重查询）
    let mut by_session: HashMap<Uuid, Vec<&MemoryL1>> = HashMap::new();
    for l1 in &unbound {
        by_session.entry(l1.session_id).or_default().push(l1);
    }

    // 3. 解析每个 session 的归属 persona_uid（逐 session 查询，失败记 warn 不中断）
    let mut session_owner: HashMap<Uuid, Option<String>> = HashMap::with_capacity(by_session.len());
    for sid in by_session.keys() {
        let owner = match storage.get_session(*sid).await {
            Ok(Some(s)) => s.persona_uid,
            Ok(None) => {
                warn!(session_id = %sid, "L2 无主 L1 处理：来源 session 不存在，无法归属");
                None
            }
            Err(e) => {
                warn!(session_id = %sid, error = %e, "L2 无主 L1 处理：查询来源 session 失败，无法归属");
                None
            }
        };
        session_owner.insert(*sid, owner);
    }

    // 4. 按归属 persona 聚合（无法归属的计入统计，保持无主状态）
    let mut by_persona: HashMap<String, Vec<&MemoryL1>> = HashMap::new();
    for (sid, l1s) in &by_session {
        match session_owner.get(sid).and_then(|o| o.as_ref()) {
            Some(owner) => {
                let entry = by_persona.entry(owner.clone()).or_default();
                entry.extend(l1s.iter().copied());
                stats.attributed += l1s.len();
            }
            None => {
                stats.unattributable += l1s.len();
            }
        }
    }

    if by_persona.is_empty() {
        debug!(
            unattributable = stats.unattributable,
            "L2 无主 L1 处理：无任何可归属候选，结束"
        );
        return stats;
    }

    // 5. 懒加载 persona 列表（确定对话另一方名称，仅当存在归属候选时查询）
    let personas = match storage.list_personas().await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "L2 无主 L1 处理：查询 persona 列表失败，另一方名称为空");
            Vec::new()
        }
    };

    let now = now_ms();
    let ms_per_day: i64 = 86_400_000;

    // 6. 逐 persona 检查触发条件并执行 L2 提取
    for (owner, l1s) in &by_persona {
        if shutdown_requested(shutdown) {
            warn!("L2 无主 L1 处理：收到停止信号，中断后续处理");
            break;
        }

        // 计数触发（路径 A）与时间触发（路径 B）独立判定
        let count_ok = trigger_count > 0 && l1s.len() >= trigger_count;
        let oldest_age_days = l1s
            .iter()
            .map(|l| l.created_at)
            .min()
            .map(|min| (now.saturating_sub(min)) as f64 / ms_per_day as f64)
            .unwrap_or(0.0);
        let age_ok = trigger_days > 0.0 && oldest_age_days >= trigger_days;

        if !(count_ok || age_ok) {
            stats.pending_groups += 1;
            info!(
                persona_uid = %owner,
                l1_count = l1s.len(),
                oldest_days = %format!("{oldest_age_days:.1}"),
                trigger_count,
                trigger_days,
                "L2 无主 L1 处理：触发条件未满足，保持无主状态待下次检查"
            );
            continue;
        }

        // 回填 persona_uid（幂等：仅更新仍为 NULL 且未吸收的记录）
        let ids: Vec<Uuid> = l1s.iter().map(|l| l.id).collect();
        match storage.assign_l1_persona_uid(&ids, owner).await {
            Ok(assigned) => {
                info!(
                    persona_uid = %owner,
                    assigned,
                    total = ids.len(),
                    "L2 无主 L1 处理：已归属 {} 条无主 L1 到 persona（{} 条跳过，可能已归属/已吸收）",
                    assigned,
                    ids.len().saturating_sub(assigned)
                );
            }
            Err(e) => {
                error!(
                    persona_uid = %owner,
                    error = %e,
                    "L2 无主 L1 处理：归属失败，跳过该组（不阻塞其他组）"
                );
                continue;
            }
        }

        // 确定对话另一方名称（仅当 personas 恰好 2 个时可靠）
        let other_name = if personas.len() == 2 {
            personas
                .iter()
                .find(|p| p.uid.as_str() != owner.as_str())
                .map(|p| p.name.clone())
        } else {
            None
        };

        stats.triggered_personas += 1;
        info!(
            persona_uid = %owner,
            l1_count = l1s.len(),
            "L2 无主 L1 处理：触发条件满足，启动事件提取（数据断层修复）"
        );
        run_l2_extraction(engine, shutdown, owner, other_name).await;
    }

    stats
}
