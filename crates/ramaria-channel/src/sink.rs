//! crates/ramaria-channel/src/sink.rs - Ramaria 通道主动投递接收端
//!
//! 设计特点:
//! - 职责单一：按通道查找已装配的传输、向目标外部会话发送文本、可重试错误退避重试
//! - 出站文本拆条：多气泡契约分隔符切段，逐段分次发送（段间间隔 + 段数上限）
//! - 不持有 Engine：依赖在构造时以「通道名 → 传输」映射注入，避免宿主侧引用环
//! - 绑定与开关门禁由服务层调度在投递前完成，本端不做业务判定
//! - 最终失败返回错误（调度侧按平台失败不记账）；日志脱敏，不记消息内容

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_service::{ProactiveMessage, ProactiveSink};

use crate::runtime::outbound::{fold_segment_overflow, split_outbound_segments};
use crate::transport::{OutboundMessage, OutboundTarget, Transport};

// =========================================================
// 通道主动投递接收端
// =========================================================

/// 通道主动投递接收端（ramaria-channel 实现）。
///
/// 职责:
/// - 按消息通道查找已装配的传输，向目标外部会话发送文本；
/// - 可重试错误按配置退避重试，最终失败返回错误（调度侧不记账）；
/// - 绑定与开关门禁由服务层调度在投递前完成，本端不做业务判定。
pub struct ChannelProactiveSink {
    transports: HashMap<String, Arc<dyn Transport>>,
    retry_delay: Duration,
    max_attempts: u32,
    segment_interval: Duration,
    segment_max: usize,
}

impl ChannelProactiveSink {
    /// 以通道名 → 传输映射构造（重试默认 1s × 2 次、分段默认 800ms × 5 段，
    /// 与 worker 出站口径一致）。
    pub fn new(transports: HashMap<String, Arc<dyn Transport>>) -> Self {
        Self::with_segment_options(
            transports,
            Duration::from_secs(1),
            2,
            Duration::from_millis(800),
            5,
        )
    }

    /// 测试注入：自定义重试间隔与最大尝试次数（>=1 次）；分段取默认值。
    pub fn with_options(
        transports: HashMap<String, Arc<dyn Transport>>,
        retry_delay: Duration,
        max_attempts: u32,
    ) -> Self {
        Self::with_segment_options(
            transports,
            retry_delay,
            max_attempts,
            Duration::from_millis(800),
            5,
        )
    }

    /// 全量注入：重试（间隔 / 上限）与分段（段间间隔 / 段数上限）一并指定。
    pub fn with_segment_options(
        transports: HashMap<String, Arc<dyn Transport>>,
        retry_delay: Duration,
        max_attempts: u32,
        segment_interval: Duration,
        segment_max: usize,
    ) -> Self {
        Self {
            transports,
            retry_delay,
            max_attempts: max_attempts.max(1),
            segment_interval,
            segment_max,
        }
    }
}

#[async_trait]
impl ProactiveSink for ChannelProactiveSink {
    /// 投递一条通道主动消息。
    ///
    /// 流程:
    /// 1. 内容与外部标识校验（空内容 / 缺外部标识为校验错误）；
    /// 2. 按通道查找传输（未装配为校验错误）；
    /// 3. 出站拆条：多气泡契约分隔符 `||` 切段（段数上限折叠）；
    /// 4. 逐段发送：段间按分段间隔等待；每段可重试错误按退避重试至上限，
    ///    不可重试错误与耗尽重试均失败返回（剩余段不再发送，已发送段不回滚）。
    ///
    /// 返回:
    /// - `Ok(())`: 全部分段已由传输发出（含拆条后为空跳过发送）；
    /// - `Err`: 校验失败或分段发送失败（文案含通道名与错误分类，不含消息内容）。
    async fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        if message.content.trim().is_empty() {
            return Err(RamariaError::validation("主动消息内容为空"));
        }
        let external_ref = message
            .external_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| RamariaError::validation("主动消息缺少外部会话标识"))?;
        let Some(transport) = self.transports.get(&message.channel) else {
            return Err(RamariaError::validation(format!(
                "通道未装配：{}",
                message.channel
            )));
        };

        let target = OutboundTarget {
            external_ref: external_ref.to_string(),
        };
        // 出站兜底：多气泡契约分隔符 `||` 切段，逐段分次发送（库内消息原文不改）
        let segments: Vec<String> = split_outbound_segments(&message.content)
            .into_iter()
            .map(|segment| segment.trim().to_string())
            .filter(|segment| !segment.is_empty())
            .collect();
        let segments = fold_segment_overflow(segments, self.segment_max);
        if segments.is_empty() {
            tracing::warn!(
                channel = %message.channel,
                external_ref = %mask_id(external_ref),
                message_id = %message.message_id,
                "净化后主动消息内容为空，跳过发送"
            );
            return Ok(());
        }

        let total = segments.len();
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(self.segment_interval).await;
            }
            let outbound = OutboundMessage::text(segment.clone());
            let mut attempt = 1;
            loop {
                match transport.send(&target, &outbound).await {
                    Ok(()) => {
                        tracing::info!(
                            channel = %message.channel,
                            external_ref = %mask_id(external_ref),
                            message_id = %message.message_id,
                            segment_index = index,
                            segment_total = total,
                            "通道主动消息分段已发送"
                        );
                        break;
                    }
                    Err(error) => {
                        let retryable = error.is_retryable();
                        if retryable && attempt < self.max_attempts {
                            tracing::warn!(
                                channel = %message.channel,
                                attempt,
                                segment_index = index,
                                segment_total = total,
                                error = %error,
                                "通道主动消息分段发送失败，准备重试"
                            );
                            attempt += 1;
                            tokio::time::sleep(self.retry_delay).await;
                            continue;
                        }
                        tracing::error!(
                            channel = %message.channel,
                            attempt,
                            retryable,
                            segment_index = index,
                            segment_total = total,
                            error = %error,
                            "通道主动消息分段发送失败，剩余段不再发送（已发送段不回滚）"
                        );
                        return Err(RamariaError::io(
                            format!(
                                "通道 {} 主动消息发送失败（{}）: {error}",
                                message.channel,
                                error.kind()
                            ),
                            None,
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
