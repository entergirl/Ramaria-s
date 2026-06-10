//! rust/crates/ramaria-app/src/setup.rs - 首次配置流程
//!
//! 设计特点:
//! - `check_setup_status`: 诊断当前配置状态，返回缺失项列表
//! - `run_setup`: 执行自动化设置步骤（保存后端配置、初始化索引）
//! - 支持幂等：重复调用不会覆盖已有有效配置
//! - 状态流: NeedsSetup → (backend可选 → privacy可选) → Indexing → Ready
//!
//! 安全约束:
//! - 不在此模块中处理 API key（keychain 由上层 CLI/Desktop 管理）
//! - 后端配置不含敏感信息（仅 provider/model/base_url）

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{AppState, BackendConfig};

// =========================================================
// 设置检查结果
// =========================================================

/// 设置检查结果——列出当前缺失的配置项。
///
/// 职责:
/// - 供 CLI/Desktop 展示设置向导步骤。
/// - `is_complete` 为 true 时表示可进入 Ready 状态。
#[derive(Debug, Clone)]
pub struct SetupStatus {
    /// 后端配置是否已保存
    pub backend_configured: bool,
    /// 是否选择了模型
    pub model_selected: bool,
    /// 索引是否需要构建
    pub needs_indexing: bool,
}

impl SetupStatus {
    /// 所有必需项是否已就绪。
    pub fn is_complete(&self) -> bool {
        self.backend_configured && self.model_selected && !self.needs_indexing
    }

    /// 缺失项的人类可读描述列表。
    pub fn missing_items(&self) -> Vec<&'static str> {
        let mut items = Vec::new();
        if !self.backend_configured {
            items.push("后端配置未完成（需选择 LLM provider）");
        }
        if !self.model_selected {
            items.push("模型未选择（需指定使用的模型）");
        }
        if self.needs_indexing {
            items.push("记忆索引待构建");
        }
        items
    }
}

// =========================================================
// 设置流程
// =========================================================

/// 诊断当前设置状态。
///
/// 参数:
/// - `storage`: 存储后端。
///
/// 返回:
/// - `SetupStatus` 描述当前配置完整度。
///
/// 检查项:
/// 1. 后端配置：`storage.get_backend_config()` 是否有记录
/// 2. 模型选择：线上 provider 检查 model_id 非空；本地 provider（LM Studio）自动通过
/// 3. 索引状态：`storage.get_index_version()` 是否为 0（0 表示未构建）
pub async fn check_setup_status(
    storage: &(dyn StorageBackend + Send + Sync),
) -> RamariaResult<SetupStatus> {
    let backend_config = storage.get_backend_config().await?;

    let backend_configured = backend_config.is_some();
    let model_selected = backend_config
        .as_ref()
        .map(|c| {
            // 本地 provider（LM Studio）允许空 model_id（用户在 LM Studio 中选择模型）
            // 线上 provider 必须有明确的 model_id
            if c.provider.is_online() {
                !c.capability.model_id.is_empty()
            } else {
                true
            }
        })
        .unwrap_or(false);

    let index_version = storage.get_index_version().await?;
    let needs_indexing = index_version == 0;

    tracing::debug!(
        backend_configured,
        model_selected,
        index_version,
        needs_indexing,
        "设置状态检查完成"
    );

    Ok(SetupStatus {
        backend_configured,
        model_selected,
        needs_indexing,
    })
}

/// 根据设置状态确定 AppState。
///
/// 参数:
/// - `status`: 设置检查结果。
///
/// 返回:
/// - `NeedsSetup`: 后端配置或模型选择未完成。
/// - `Indexing`: 索引待构建。
/// - `Ready`: 全部就绪。
pub fn determine_state(status: &SetupStatus) -> AppState {
    if !status.backend_configured || !status.model_selected {
        AppState::NeedsSetup
    } else if status.needs_indexing {
        AppState::Indexing
    } else {
        AppState::Ready
    }
}

/// 保存后端配置。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: 要保存的后端配置（不含 API key）。
///
/// 返回:
/// - `Ok(())`: 保存成功。
///
/// 说明:
/// - 重复调用会覆盖已有配置。
/// - LM Studio 场景下 model_id 可为空（用户在 LM Studio 中选择）。
pub async fn save_backend_config(
    storage: &(dyn StorageBackend + Send + Sync),
    config: &BackendConfig,
) -> RamariaResult<()> {
    storage.save_backend_config(config).await?;

    tracing::info!(
        provider = %config.provider,
        model = %config.capability.model_id,
        base_url = %config.base_url,
        "后端配置已保存"
    );

    Ok(())
}

/// 标记索引构建完成。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `version`: 索引版本号（通常为 1）。
///
/// 用途:
/// - 索引重建完成后调用，将 `Indexing` 状态转为 `Ready`。
pub async fn mark_index_ready(
    storage: &(dyn StorageBackend + Send + Sync),
    version: i32,
) -> RamariaResult<()> {
    storage.set_index_version(version).await?;

    tracing::info!(version, "索引版本已更新");

    Ok(())
}

/// 完整设置流程：保存后端配置 → 标记索引。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: 用户选择的后端配置。
///
/// 返回:
/// - `Ok(AppState::Ready)`: 设置完成。
///
/// 说明:
/// - 调用后状态从 `NeedsSetup` → `Ready`。
/// - 实际索引构建由 memory 层的 `IndexRebuilder` 另行执行。
pub async fn run_setup(
    storage: &(dyn StorageBackend + Send + Sync),
    config: &BackendConfig,
) -> RamariaResult<AppState> {
    // Step 1: 保存后端配置
    save_backend_config(storage, config).await?;

    // Step 2: 检查索引状态
    let status = check_setup_status(storage).await?;

    if status.needs_indexing {
        tracing::info!("索引待构建，建议运行 rebuild_index");
    }

    // Step 3: 确定最终状态
    let state = determine_state(&status);
    tracing::info!(%state, "设置流程完成");

    Ok(state)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_status_complete() {
        let status = SetupStatus {
            backend_configured: true,
            model_selected: true,
            needs_indexing: false,
        };
        assert!(status.is_complete());
        assert!(status.missing_items().is_empty());
    }

    #[test]
    fn setup_status_missing_backend() {
        let status = SetupStatus {
            backend_configured: false,
            model_selected: false,
            needs_indexing: false,
        };
        assert!(!status.is_complete());
        assert_eq!(status.missing_items().len(), 2);
    }

    #[test]
    fn setup_status_needs_indexing() {
        let status = SetupStatus {
            backend_configured: true,
            model_selected: true,
            needs_indexing: true,
        };
        assert!(!status.is_complete());
        assert_eq!(status.missing_items().len(), 1);
        assert!(status.missing_items()[0].contains("索引"));
    }

    #[test]
    fn determine_state_needs_setup() {
        let status = SetupStatus {
            backend_configured: false,
            model_selected: false,
            needs_indexing: false,
        };
        assert_eq!(determine_state(&status), AppState::NeedsSetup);
    }

    #[test]
    fn determine_state_indexing() {
        let status = SetupStatus {
            backend_configured: true,
            model_selected: true,
            needs_indexing: true,
        };
        assert_eq!(determine_state(&status), AppState::Indexing);
    }

    #[test]
    fn determine_state_ready() {
        let status = SetupStatus {
            backend_configured: true,
            model_selected: true,
            needs_indexing: false,
        };
        assert_eq!(determine_state(&status), AppState::Ready);
    }
}
