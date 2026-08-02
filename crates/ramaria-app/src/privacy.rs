//! rust/crates/ramaria-app/src/privacy.rs - 隐私确认流程
//!
//! 设计特点:
//! - 按 provider + base_url 粒度检查隐私确认状态
//! - `check_privacy` 返回是否需要确认，以及已有的确认记录
//! - `confirm_privacy` 记录用户同意并持久化到 storage
//! - 本地 provider（LM Studio）自动通过，不需要隐私确认
//! - provider 或 base_url 变更时需重新确认
//!
//! 安全约束:
//! - 隐私确认仅记录决策（同意/不同意），不涉及 API key
//! - persistent=true 表示跨重启有效，false 表示仅本次会话有效

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{LlmProvider, PrivacyConsent};

// =========================================================
// 隐私检查结果
// =========================================================

/// 隐私确认检查结果。
///
/// 变体:
/// - `NotNeeded`: 本地 provider，无需确认
/// - `Confirmed`: 已有有效确认记录
/// - `NeedsConfirmation`: 需要用户确认（首次使用或 base_url 变更）
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PrivacyStatus {
    /// 本地服务，不需要隐私确认
    NotNeeded,
    /// 已确认（含确认记录）
    Confirmed {
        /// 是否跨重启持久化
        persistent: bool,
        /// 确认时间（Unix 毫秒）
        confirmed_at: i64,
    },
    /// 需要用户确认
    NeedsConfirmation {
        /// provider 名称（用于 UI 展示）
        provider_name: String,
        /// 服务地址（用于 UI 展示）
        base_url: String,
    },
}

impl PrivacyStatus {
    /// 是否需要显示确认对话框。
    pub fn needs_user_action(&self) -> bool {
        matches!(self, Self::NeedsConfirmation { .. })
    }

    /// 是否已确认（包括本地无需确认的情况）。
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::NotNeeded | Self::Confirmed { .. })
    }
}

// =========================================================
// 隐私确认流程
// =========================================================

/// 检查指定 provider + base_url 的隐私确认状态。
///
/// 参数:
/// - `storage`: 存储后端，用于查询已有确认记录。
/// - `provider`: LLM provider。
/// - `base_url`: API 基础地址。
///
/// 返回:
/// - `NotNeeded`: provider 为本地（LmStudio），无需确认。
/// - `Confirmed`: 已有有效记录。
/// - `NeedsConfirmation`: 需用户确认。
///
/// 说明:
/// - 线上 provider（DeepSeek/OpenAI）若无确认记录，返回 `NeedsConfirmation`。
/// - 不在此函数中记录日志，由调用方根据返回结果决定 UI 行为。
pub async fn check_privacy(
    storage: &(dyn StorageBackend + Send + Sync),
    provider: LlmProvider,
    base_url: &str,
) -> RamariaResult<PrivacyStatus> {
    // 本地 provider 自动通过
    if !provider.is_online() {
        return Ok(PrivacyStatus::NotNeeded);
    }

    // 查询已有确认记录
    let consent = storage
        .get_privacy_consent(provider.as_str(), base_url)
        .await?;

    match consent {
        Some(c) => {
            tracing::debug!(
                provider = %provider,
                %base_url,
                persistent = c.persistent,
                "隐私确认已存在"
            );
            Ok(PrivacyStatus::Confirmed {
                persistent: c.persistent,
                confirmed_at: c.timestamp,
            })
        }
        None => {
            tracing::info!(
                provider = %provider,
                %base_url,
                "需要用户进行隐私确认"
            );
            Ok(PrivacyStatus::NeedsConfirmation {
                provider_name: provider.to_string(),
                base_url: base_url.to_string(),
            })
        }
    }
}

/// 记录用户隐私确认决策。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `provider`: LLM provider。
/// - `base_url`: API 基础地址。
/// - `persistent`: 是否跨重启持久化（勾选"下次不再提醒"）。
///
/// 返回:
/// - `Ok()`: 记录成功。
/// - `Err`: 存储写入失败。
///
/// 安全约束:
/// - 仅记录 provider + base_url + 时间戳 + persistent 标记，不记录 API key。
pub async fn confirm_privacy(
    storage: &(dyn StorageBackend + Send + Sync),
    provider: LlmProvider,
    base_url: &str,
    persistent: bool,
) -> RamariaResult<()> {
    let consent = PrivacyConsent::new(provider, base_url.to_string(), persistent);

    storage.save_privacy_consent(&consent).await?;

    tracing::info!(
        provider = %provider,
        %base_url,
        persistent,
        "隐私确认已记录"
    );

    Ok(())
}

/// 断言隐私确认已完成，否则返回错误。
///
/// 用途:
/// - `send_message` 等需要调用线上 LLM 的操作前，调用此函数快速检查。
/// - 若确认未完成，返回 `RamariaError::Privacy`，上层可转换为 UI 提示。
pub async fn require_privacy(
    storage: &(dyn StorageBackend + Send + Sync),
    provider: LlmProvider,
    base_url: &str,
) -> RamariaResult<()> {
    let status = check_privacy(storage, provider, base_url).await?;

    match status {
        PrivacyStatus::NotNeeded | PrivacyStatus::Confirmed { .. } => Ok(()),
        PrivacyStatus::NeedsConfirmation {
            provider_name,
            base_url,
        } => Err(RamariaError::privacy(format!(
            "使用线上 LLM 服务 ({provider_name}, {base_url}) 前需要完成隐私确认。请先在设置中确认同意将对话内容发送到线上服务。"
        ))),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_status_variants() {
        // NotNeeded: 无需用户操作、已确认
        let status = PrivacyStatus::NotNeeded;
        assert!(!status.needs_user_action());
        assert!(status.is_confirmed());
        // Confirmed: 无需用户操作、已确认
        let status = PrivacyStatus::Confirmed {
            persistent: true,
            confirmed_at: 1000,
        };
        assert!(!status.needs_user_action());
        assert!(status.is_confirmed());
        // NeedsConfirmation: 需要用户操作、未确认
        let status = PrivacyStatus::NeedsConfirmation {
            provider_name: "DeepSeek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
        };
        assert!(status.needs_user_action());
        assert!(!status.is_confirmed());
    }

    #[test]
    fn provider_online_status() {
        let cases = [
            (LlmProvider::LmStudio, false),
            (LlmProvider::DeepSeek, true),
            (LlmProvider::OpenAI, true),
        ];
        for (provider, expected) in cases {
            assert_eq!(provider.is_online(), expected, "{provider:?}");
        }
    }
}
