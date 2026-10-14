//! crates/ramaria-service/src/engine/assemble/providers.rs - Ramaria provider 构造与嵌入恢复
//!
//! 设计特点:
//! - 装配与后端配置热更新共用同一 provider 构造口径（缓存注入条件一致）
//! - 嵌入模型恢复失败降级（缺失 / 加载失败均返回 None，不阻塞装配）

use std::path::Path;
use std::sync::Arc;

use ramaria_core::config::EmbeddingDevice;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind};
use ramaria_llm::keychain::Keychain;

use super::path_log_label;

/// 按后端配置构建 LLM provider（可选注入响应缓存）。
///
/// 参数:
/// - `backend_config`: 后端配置（provider / base_url / model）。
/// - `keychain`: 线上 provider 的 API key 来源（本地 provider 不使用）。
/// - `cache`: 精确缓存（`[cache].enabled=false` 时为 None）。
///
/// 说明:
/// - 装配与后端配置热更新共用本函数，保证两处的 provider 构造口径与缓存注入条件一致。
pub(crate) fn build_llm_provider(
    backend_config: &BackendConfig,
    keychain: &Arc<Keychain>,
    cache: Option<Arc<dyn LlmResponseCache>>,
) -> RamariaResult<Arc<dyn LlmProvider>> {
    let provider: Arc<dyn LlmProvider> = match backend_config.provider {
        LlmProviderKind::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(backend_config.clone())?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        LlmProviderKind::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                backend_config.clone(),
                Arc::clone(keychain),
            )?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        LlmProviderKind::OpenAI => {
            let provider = ramaria_llm::openai::OpenAIProvider::new(
                backend_config.clone(),
                Arc::clone(keychain),
            )?;
            match cache {
                Some(cache) => Arc::new(provider.with_cache(cache)),
                None => Arc::new(provider),
            }
        }
        // non_exhaustive 兜底：未知 provider 显式报错，不静默退化
        other => {
            return Err(RamariaError::unsupported(format!(
                "不支持的 LLM provider: {}",
                other.as_str()
            )));
        }
    };
    Ok(provider)
}

/// 尝试恢复已保存的嵌入模型；缺失 / 加载失败返回 None（向量通道降级）。
///
/// 说明:
/// - 模型目录存在但加载失败（文件损坏 / 设备不可用）同样降级，
///   记 warn 不阻塞装配（BM25 + 关键词镜像继续工作）。
pub(super) fn restore_embedding(
    backend_config: &BackendConfig,
    device: EmbeddingDevice,
) -> Option<Arc<dyn EmbeddingProvider>> {
    let saved_path = backend_config
        .embedding_model_path
        .as_deref()
        .filter(|path| !path.is_empty())?;

    let model_dir = Path::new(saved_path);
    if !model_dir.exists() {
        tracing::warn!(
            path = %path_log_label(model_dir),
            "已保存的嵌入模型目录不存在，向量通道降级（BM25 + 关键词镜像继续可用）"
        );
        return None;
    }

    match ramaria_llm::embedding::native::create_native_provider_with_device(model_dir, device) {
        Ok(provider) => {
            let info = provider.model_info();
            tracing::info!(
                path = %path_log_label(model_dir),
                model_id = %info.model_id,
                dim = info.dimension,
                device = device.as_str(),
                "已恢复嵌入模型（向量通道可用）"
            );
            Some(Arc::new(provider) as Arc<dyn EmbeddingProvider>)
        }
        Err(e) => {
            tracing::warn!(
                path = %path_log_label(model_dir),
                error = %e,
                "加载已保存的嵌入模型失败，向量通道降级（BM25 + 关键词镜像继续可用）"
            );
            None
        }
    }
}
