//! crates/ramaria-service/src/privacy.rs - 隐私确认流程
//!
//! 设计特点:
//! - 按 provider + base_url 粒度检查隐私确认状态
//! - `check_privacy` 返回是否需要确认，以及已有的确认记录
//! - `confirm_privacy` 记录用户同意并持久化到 storage
//! - 本地 provider（LM Studio）自动通过，不需要隐私确认
//! - provider 或 base_url 变更时需重新确认
//! - 引擎门面（[`check`] / [`confirm`]）的判定输入取 DB 侧后端配置，
//!   与桌面 / CLI 现状同源（无记录按本地 provider 默认值）
//!
//! 安全约束:
//! - 隐私确认仅记录决策（同意/不同意），不涉及 API key
//! - persistent=true 表示跨重启有效，false 表示仅本次会话有效

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{BackendConfig, LlmProvider, PrivacyConsent};

use crate::engine::Engine;

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
/// - 需要调用线上 LLM 的操作前，调用此函数快速检查。
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
// 引擎门面（判定输入取 DB 侧后端配置）
// =========================================================

/// 检查当前后端的隐私确认状态。
///
/// 说明:
/// - 判定输入（provider / base_url）取 DB 侧 `backend_config`，与桌面 / CLI 现状同源；
/// - 无后端配置记录时按本地 provider 默认值判定（无需确认）。
pub(crate) async fn check(engine: &Engine) -> RamariaResult<PrivacyStatus> {
    let backend = effective_backend_config(engine).await?;
    check_privacy(
        engine.storage_ref().as_ref(),
        backend.provider,
        &backend.base_url,
    )
    .await
}

/// 记录当前后端的隐私确认（provider / base_url 取 DB 侧后端配置）。
///
/// 参数:
/// - `persistent`: 是否跨重启持久化（勾选"下次不再提醒"）。
pub(crate) async fn confirm(engine: &Engine, persistent: bool) -> RamariaResult<()> {
    let backend = effective_backend_config(engine).await?;
    confirm_privacy(
        engine.storage_ref().as_ref(),
        backend.provider,
        &backend.base_url,
        persistent,
    )
    .await
}

/// 线上 provider 的隐私确认判定（主动调度与主动生成共用的只读门禁）。
///
/// 返回:
/// - `Ok(true)`: 本地 provider，或线上 provider 已完成确认（放行）；
/// - `Ok(false)`: 线上 provider 未完成确认（调用方静默跳过）；
/// - `Err`: 隐私记录读取失败。
pub(crate) async fn online_privacy_confirmed(engine: &Engine) -> RamariaResult<bool> {
    let backend = engine.llm_ref().config().clone();
    if !backend.provider.is_online() {
        return Ok(true);
    }
    let status = check_privacy(
        engine.storage_ref().as_ref(),
        backend.provider,
        &backend.base_url,
    )
    .await?;
    Ok(status.is_confirmed())
}

/// DB 侧后端配置（无记录回退本地 provider 默认值）。
async fn effective_backend_config(engine: &Engine) -> RamariaResult<BackendConfig> {
    Ok(engine
        .backend_config()
        .await?
        .unwrap_or_else(BackendConfig::lm_studio_default))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::StoreInfrastructure;

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

    /// 引擎门面：本地 provider（无后端配置记录，缺省回退）无需确认。
    #[tokio::test]
    async fn engine_check_local_provider_not_needed() {
        let (engine, _storage, dir) = crate::test_support::engine_with_db("privacy-local").await;

        let status = engine.check_privacy().await.expect("检查应成功");
        assert_eq!(status, PrivacyStatus::NotNeeded);
        assert!(status.is_confirmed());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 引擎门面：线上 provider 未确认 → NeedsConfirmation（含展示信息）。
    #[tokio::test]
    async fn engine_check_online_provider_needs_confirmation() {
        let (engine, storage, dir) = crate::test_support::engine_with_db("privacy-online").await;
        storage
            .save_backend_config(&BackendConfig::deepseek_default())
            .await
            .expect("保存后端配置应成功");

        let status = engine.check_privacy().await.expect("检查应成功");
        match status {
            PrivacyStatus::NeedsConfirmation {
                provider_name,
                base_url,
            } => {
                assert_eq!(provider_name, "deepseek", "展示名取 provider 稳定标识");
                assert_eq!(base_url, "https://api.deepseek.com/v1");
            }
            other => panic!("应为 NeedsConfirmation，实际 {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 引擎门面：确认后返回 Confirmed，persistent 标记与确认记录一致。
    #[tokio::test]
    async fn engine_confirm_privacy_marks_confirmed() {
        let (engine, storage, dir) = crate::test_support::engine_with_db("privacy-confirmed").await;
        storage
            .save_backend_config(&BackendConfig::deepseek_default())
            .await
            .expect("保存后端配置应成功");

        engine.confirm_privacy(true).await.expect("确认应成功");
        match engine.check_privacy().await.expect("检查应成功") {
            PrivacyStatus::Confirmed {
                persistent,
                confirmed_at,
            } => {
                assert!(persistent);
                assert!(confirmed_at > 0);
            }
            other => panic!("确认后应为 Confirmed，实际 {other:?}"),
        }

        // 确认记录按 provider + base_url 粒度落库
        let consent = storage
            .get_privacy_consent("deepseek", "https://api.deepseek.com/v1")
            .await
            .expect("读取确认记录应成功")
            .expect("确认记录应存在");
        assert!(consent.persistent);

        // 临时确认（persistent=false）：语义如实回传
        let (engine2, storage2, dir2) = crate::test_support::engine_with_db("privacy-temp").await;
        storage2
            .save_backend_config(&BackendConfig::openai_default())
            .await
            .expect("保存后端配置应成功");
        engine2.confirm_privacy(false).await.expect("确认应成功");
        match engine2.check_privacy().await.expect("检查应成功") {
            PrivacyStatus::Confirmed { persistent, .. } => assert!(!persistent),
            other => panic!("确认后应为 Confirmed，实际 {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
