//! crates/ramaria-service/src/import/l1.rs - Ramaria 导入 L1 批量生成与 ETA 进度模块
//!
//! 设计特点:
//! - 双口径：逐 session × 逐目标调用 L1 生成（`cascade=true` 走带级联口径，否则无级联口径）；
//!   群聊多画像分发时每会话一次生成（按块内参与者复制 L1 行，忽略 targets）
//! - 请求间节流：连续 LLM 调用按 `plan.throttle_ms` 保持最小间隔（0 = 不等待）
//! - 分层 EMA 预估（`crate::eta`）：循环前发起始进度，每完成一个 session 推送一条（含剩余秒数）
//! - 静默降级：单次生成失败只记 warn 并计入失败计数，不中断批量
//! - `done_summary` 构造完成摘要（两种分支文案与桌面 done 事件一致）

use std::time::Instant;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::MemoryL1;
use uuid::Uuid;

use crate::engine::Engine;
use crate::eta::{EtaEstimator, PhaseKind};

// =========================================================
// 请求与结果类型
// =========================================================

/// L1 批量生成计划。
///
/// 字段约定:
/// - `targets`: 每个目标对应一次 L1 生成（`None` = 不绑定画像，`persona_uid` 存 NULL）；
/// - `l1_prefix`: L1 角色前缀覆盖（`None` = 摘要器默认「用户：/助手：」；
///   `Some((user, assistant))` = 覆盖，导入消息自带人名前缀时传空串）；
/// - `cascade`: `true` 时每次生成末尾触发 L2 检查（宿主自行汇总触发），`false` 为无级联口径；
/// - `throttle_ms`: 连续 LLM 调用之间的最小间隔（毫秒，0 = 不等待）；
/// - `group_fanout`: 群聊多画像分发：每会话一次生成 × 块内参与者复制 L1 行；
///   为 true 时忽略 `targets` 与 `l1_prefix`（分发恒用空前缀）。
#[derive(Debug, Clone)]
pub struct ImportL1Plan {
    /// L1 生成目标列表（每个目标一次调用）
    pub targets: Vec<Option<String>>,
    /// L1 角色前缀覆盖（为 `None` 时用摘要器默认前缀）
    pub l1_prefix: Option<(String, String)>,
    /// 是否在每次生成末尾触发 L2 检查
    pub cascade: bool,
    /// 连续 LLM 调用间最小间隔（毫秒）
    pub throttle_ms: u64,
    /// 群聊多画像分发（为 true 时忽略 `targets` 与 `l1_prefix`）
    pub group_fanout: bool,
}

/// L1 批量生成结果。
#[derive(Debug, Clone)]
pub struct ImportL1Outcome {
    /// 生成成功数
    pub l1_success: usize,
    /// 生成失败数
    pub l1_failed: usize,
    /// 跳过数（会话无消息或已有同画像摘要）
    pub l1_skipped: usize,
    /// 实际处理次数（成功 + 跳过 + 失败）
    pub l1_processed: usize,
    /// 计划调用总数（session 数 × 目标数）
    pub l1_total: usize,
    /// 参与的 session UUID 列表
    pub session_ids: Vec<Uuid>,
}

/// 导入进度事件（宿主转发为自身事件通道）。
///
/// 字段约定:
/// - `phase`: 阶段字符串（`l1` / `l2` / `l3`）；
/// - `current` / `total`: 阶段内进度（总数 0 表示未知）；
/// - `eta_seconds`: 分层 EMA 估算的剩余秒数（None = 无样本，宿主可回退线性估算）；
/// - `l1_total` / `l2_total` / `l3_total`: 各阶段预计总量（None = 当前阶段未知）。
#[derive(Debug, Clone)]
pub struct ImportL1Progress {
    /// 阶段: "l1" | "l2" | "l3"
    pub phase: &'static str,
    /// 当前进度（已处理数）
    pub current: usize,
    /// 总数（0 表示未知）
    pub total: usize,
    /// 人类可读的阶段描述
    pub message: String,
    /// 分层 EMA 估算的剩余秒数
    pub eta_seconds: Option<u64>,
    /// L1 阶段预计总量
    pub l1_total: Option<usize>,
    /// L2 阶段预计总量
    pub l2_total: Option<usize>,
    /// L3 阶段预计总量
    pub l3_total: Option<usize>,
}

/// 导入完成摘要（宿主在完成事件中携带）。
#[derive(Debug, Clone)]
pub struct ImportDoneSummary {
    /// L1 生成成功数
    pub l1_success: usize,
    /// L1 生成失败数
    pub l1_failed: usize,
    /// 深度模式：L2 是否已触发
    pub l2_triggered: bool,
    /// 深度模式：L3 是否已触发
    pub l3_triggered: bool,
    /// 导入的 session 总数
    pub total_sessions: usize,
    /// 人类可读的完成消息
    pub message: String,
}

/// 导入进度回调（宿主实现并转发到自身事件通道）。
///
/// 实现要求:
/// - 回调在导入执行路径上同步调用，实现应尽快返回（不阻塞批量生成）；
/// - `on_done` 由宿主在汇总完成统计后调用（服务层不代为触发）。
pub trait ImportProgressSink: Send + Sync {
    /// L1 / L2 / L3 阶段进度。
    fn on_l1_progress(&self, p: &ImportL1Progress);

    /// 导入完成摘要。
    fn on_done(&self, s: &ImportDoneSummary);
}

// =========================================================
// 用例入口
// =========================================================

/// 批量生成导入会话的 L1 摘要。
///
/// 流程:
/// - 循环前先发一条起始进度（`current = 0`、`total = l1_total`），宿主只做转发；
/// - 逐 session × 逐目标调用 L1 生成（`cascade=true` 走带级联口径，否则无级联口径）；
/// - 每次调用后按 `plan.throttle_ms` 执行请求间节流；
/// - 每完成一个 session 更新分层 EMA 估算并经进度回调发送一条 `l1` 阶段进度。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_ids`: 待生成 L1 的会话（取 L0 导入结果）；
/// - `plan`: 目标列表与节流 / 级联选项；
/// - `progress`: 可选进度回调。
///
/// 返回:
/// - `ImportL1Outcome`：成功 / 跳过 / 失败计数与处理总数。
pub(crate) async fn generate_l1(
    engine: &Engine,
    session_ids: &[Uuid],
    plan: ImportL1Plan,
    progress: Option<&dyn ImportProgressSink>,
) -> RamariaResult<ImportL1Outcome> {
    let started_at = Instant::now();
    let mut eta = EtaEstimator::new();
    // 预计总量：群聊分发每会话一次生成；逐目标口径为 session 数 × 目标数
    let l1_total = if plan.group_fanout {
        session_ids.len()
    } else {
        session_ids.len() * plan.targets.len()
    };

    let mut l1_success = 0usize;
    let mut l1_failed = 0usize;
    let mut l1_skipped = 0usize;
    let mut l1_processed = 0usize;

    // 角色前缀：None 走摘要器默认前缀，Some 覆盖为指定文本（群聊分发分支不使用）
    let (user_prefix, assistant_prefix) = match &plan.l1_prefix {
        Some((u, a)) => (Some(u.as_str()), Some(a.as_str())),
        None => (None, None),
    };

    // 起始进度：总量已知，先给出 0/总数 的起点（宿主不再自行计算 L1 总量）
    if let Some(sink) = progress {
        let message = if plan.group_fanout {
            "正在生成 L1 会话摘要（群成员分发）..."
        } else {
            "正在生成 L1 会话摘要（双方 persona）..."
        };
        sink.on_l1_progress(&ImportL1Progress {
            phase: "l1",
            current: 0,
            total: l1_total,
            message: message.to_string(),
            eta_seconds: None,
            l1_total: Some(l1_total),
            l2_total: None,
            l3_total: None,
        });
    }

    for session_id in session_ids {
        if plan.group_fanout {
            // 群聊多画像分发：每会话一次生成，块内参与者复制成行
            match regenerate_l1_group(engine, *session_id).await {
                Ok(rows) if !rows.is_empty() => l1_success += 1,
                Ok(rows) => {
                    l1_skipped += 1;
                    tracing::debug!(
                        session_id = %session_id,
                        rows = rows.len(),
                        "群聊 L1 无内容可生成，跳过"
                    );
                }
                Err(e) => {
                    l1_failed += 1;
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "群聊 L1 摘要生成失败（非致命）"
                    );
                }
            }
            l1_processed += 1;

            // 级联语义：与逐目标口径对齐，生成后按计划触发 L2 检查
            if plan.cascade {
                engine.trigger_l2_check().await;
            }

            // 请求间节流：连续 LLM 调用间保持最小间隔，避免触发远端速率限制
            ramaria_memory::llm_gate::inter_llm_delay(plan.throttle_ms, "L1 群聊分发").await;
        } else {
            for target in &plan.targets {
                let result = if plan.cascade {
                    engine
                        .regenerate_l1(
                            *session_id,
                            target.as_deref(),
                            user_prefix,
                            assistant_prefix,
                        )
                        .await
                } else {
                    engine
                        .regenerate_l1_no_cascade(
                            *session_id,
                            target.as_deref(),
                            user_prefix,
                            assistant_prefix,
                        )
                        .await
                };

                match result {
                    Ok(Some(_)) => l1_success += 1,
                    Ok(None) => {
                        // 会话无消息或已有同画像摘要：跳过不影响连续失败计数
                        l1_skipped += 1;
                        tracing::debug!(
                            session_id = %session_id,
                            persona_uid = ?target.as_deref().map(mask_id),
                            "L1 无内容可生成，跳过"
                        );
                    }
                    Err(e) => {
                        l1_failed += 1;
                        tracing::warn!(
                            session_id = %session_id,
                            persona_uid = ?target.as_deref().map(mask_id),
                            error = %e,
                            "L1 摘要生成失败（非致命）"
                        );
                    }
                }
                l1_processed += 1;

                // 请求间节流：连续 LLM 调用间保持最小间隔，避免触发远端速率限制
                ramaria_memory::llm_gate::inter_llm_delay(plan.throttle_ms, "L1 导入批量摘要")
                    .await;
            }
        }

        // 每完成一个 session 推送一次进度（分母为 LLM 调用总次数）
        eta.update(
            PhaseKind::L1,
            l1_processed,
            l1_total,
            started_at.elapsed().as_secs_f64(),
        );
        if let Some(sink) = progress {
            let message = if plan.group_fanout {
                format!("L1 摘要 {l1_processed}/{l1_total}（群成员分发）")
            } else {
                format!("L1 摘要 {l1_processed}/{l1_total}（双方 persona）")
            };
            sink.on_l1_progress(&ImportL1Progress {
                phase: "l1",
                current: l1_processed,
                total: l1_total,
                message,
                eta_seconds: eta.remaining_seconds().map(|s| s.round() as u64),
                l1_total: Some(l1_total),
                l2_total: None,
                l3_total: None,
            });
        }
    }

    tracing::info!(
        l1_success,
        l1_failed,
        l1_skipped,
        l1_processed,
        total_sessions = session_ids.len(),
        "L1 摘要批量生成完成"
    );

    Ok(ImportL1Outcome {
        l1_success,
        l1_failed,
        l1_skipped,
        l1_processed,
        l1_total,
        session_ids: session_ids.to_vec(),
    })
}

// =========================================================
// 群聊分发生成
// =========================================================

/// 群聊会话生成分发 L1 摘要（每会话一次生成，块内参与者复制成多行）。
///
/// 流程:
/// 1. 读会话消息；无消息 → 记 warn 并返回空列表；
/// 2. 幂等：已有带 persona 归属的 L1 行（此前分发结果）→ 只补索引镜像并原样返回，不重复调用 LLM；
/// 3. 清理旧的无归属摘要（`persona_uid IS NULL`），避免新旧两份并存；
/// 4. 生成（多画像分发开启；空前缀——导入消息自带人名前缀）；
/// 5. 读回落库行，逐行增量镜像并返回。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_id`: 目标会话。
///
/// 返回:
/// - 本次实际可用的 L1 行（含此前分发结果）；空列表表示会话无消息。
pub(crate) async fn regenerate_l1_group(
    engine: &Engine,
    session_id: Uuid,
) -> RamariaResult<Vec<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        tracing::warn!(%session_id, "regenerate_l1_group: 会话无消息，跳过");
        return Ok(Vec::new());
    }

    // 幂等：带 persona 归属的 L1 行视为此前分发结果，不重复生成
    let existing = storage.list_memory_l1(session_id).await?;
    let dispatched: Vec<MemoryL1> = existing
        .into_iter()
        .filter(|l1| l1.persona_uid.is_some())
        .collect();
    if !dispatched.is_empty() {
        tracing::info!(
            %session_id,
            rows = dispatched.len(),
            "该会话已有群聊分发 L1 摘要，跳过重新生成"
        );
        for l1 in &dispatched {
            crate::index::index_l1_into_mirrors(engine, l1).await;
        }
        return Ok(dispatched);
    }

    // 清理旧的无归属摘要（只删 persona_uid IS NULL 行），再做生成
    let deleted = storage.delete_memory_l1_by_session(session_id).await?;
    if deleted > 0 {
        tracing::info!(%session_id, deleted, "已清理旧的无归属 L1 摘要");
    }

    tracing::info!(
        %session_id,
        msg_count = messages.len(),
        "群聊 L1 摘要生成（成员分发）"
    );

    crate::lifecycle::l1::generate_l1_summary(engine, session_id, None, Some(""), Some(""), true)
        .await?;

    let rows = storage.list_memory_l1(session_id).await?;
    if rows.is_empty() {
        return Err(RamariaError::validation("群聊 L1 摘要生成后无法读取"));
    }
    for l1 in &rows {
        crate::index::index_l1_into_mirrors(engine, l1).await;
    }
    tracing::info!(%session_id, rows = rows.len(), "群聊 L1 摘要生成完成（成员分发）");
    Ok(rows)
}

/// 构造导入完成摘要（两种分支的文案与桌面 done 事件一致）。
///
/// 参数:
/// - `l1`: L1 批量生成结果（提供成功 / 失败 / 处理次数三个计数，成组传入避免同型参数错位）；
/// - `l2_triggered` / `l3_triggered`: 深度阶段是否已触发；
/// - `total_sessions`: 导入会话总数。
///
/// 返回:
/// - `ImportDoneSummary`：宿主在完成事件中直接使用 `message` 与统计字段。
pub fn done_summary(
    l1: &ImportL1Outcome,
    l2_triggered: bool,
    l3_triggered: bool,
    total_sessions: usize,
) -> ImportDoneSummary {
    let message = if l1.l1_failed > 0 {
        format!(
            "深度处理完成: L1 成功 {}/{}, 失败 {}。请确认 LLM 已连接后重试。",
            l1.l1_success, l1.l1_processed, l1.l1_failed
        )
    } else {
        format!(
            "深度处理完成: L1 全部成功 ({}/{})",
            l1.l1_success, l1.l1_processed
        )
    };

    ImportDoneSummary {
        l1_success: l1.l1_success,
        l1_failed: l1.l1_failed,
        l2_triggered,
        l3_triggered,
        total_sessions,
        message,
    }
}
