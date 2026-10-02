//! crates/ramaria-service/src/config/backend_map.rs - BackendSelection 与 BackendConfig 映射
//!
//! 设计特点:
//! - 文件侧 `[backend]` 组（BackendSelection）↔ backend_config 表（BackendConfig）双向映射
//! - 业务字段（provider / base_url / model / embedding / temperature / max_tokens）以文件为准覆盖
//! - `online_memory_injection` 仅存在于文件侧（DB 无对应字段），映射时保留 fallback 值
//! - capability 派生字段随 provider / model / base_url 同步；embedding_model_path 不参与比较
//! - 比较口径忽略派生字段，避免"表 / 文件"因派生差异反复误判不一致

use ramaria_core::types::BackendConfig;

// =========================================================
// BackendSelection ↔ BackendConfig 映射
// =========================================================

/// 将 `RamariaConfig.backend`（BackendSelection）映射为 BackendConfig。
///
/// 说明:
/// - `existing` 提供 capability / embedding_model_path 基线（None 时按 provider 默认构造）。
/// - 业务字段（provider/base_url/model_id/embedding/temperature/max_tokens）以文件为准覆盖。
pub(super) fn backend_config_from_selection(
    sel: &ramaria_core::config::BackendSelection,
    existing: Option<&BackendConfig>,
) -> BackendConfig {
    let mut bc = existing.cloned().unwrap_or_else(|| {
        BackendConfig::new_with_defaults(sel.provider, sel.base_url.clone(), sel.model_id.clone())
    });
    bc.provider = sel.provider;
    bc.base_url = sel.base_url.clone();
    bc.embedding_model_id = sel.embedding_model_id.clone();
    bc.temperature = sel.temperature;
    bc.max_tokens = sel.max_tokens;
    bc.capability.provider = sel.provider;
    bc.capability.model_id = sel.model_id.clone();
    bc.capability.base_url = sel.base_url.clone();
    bc
}

/// 将 BackendConfig 映射为 `RamariaConfig.backend`（BackendSelection）。
///
/// 说明:
/// - `online_memory_injection` 仅存在于文件侧（DB 无对应字段），保留 fallback 值。
pub(super) fn backend_selection_from_backend_config(
    bc: &BackendConfig,
    fallback: &ramaria_core::config::BackendSelection,
) -> ramaria_core::config::BackendSelection {
    ramaria_core::config::BackendSelection {
        provider: bc.provider,
        model_id: bc.capability.model_id.clone(),
        base_url: bc.base_url.clone(),
        embedding_model_id: bc.embedding_model_id.clone(),
        temperature: bc.temperature,
        max_tokens: bc.max_tokens,
        online_memory_injection: fallback.online_memory_injection,
    }
}

/// 比较两个 BackendConfig 的业务字段（忽略 capability 的派生字段与 embedding_model_path）。
pub(super) fn backend_fields_equal(a: &BackendConfig, b: &BackendConfig) -> bool {
    a.provider == b.provider
        && a.base_url == b.base_url
        && a.embedding_model_id == b.embedding_model_id
        && (a.temperature - b.temperature).abs() < f64::EPSILON
        && a.max_tokens == b.max_tokens
        && a.capability.model_id == b.capability.model_id
}
