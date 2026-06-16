//! rust/crates/ramaria-app/src/app_state.rs - 应用状态管理
//!
//! 设计特点:
//! - 从 `app.rs` 提取的状态管理方法
//! - 所有方法通过 `impl App` 关联，访问 `pub(crate)` 字段
//! - 涵盖状态读写、LLM/Embedding provider 热更新、嵌入模型加载
//! - 降级策略：嵌入模型缺失或验证失败 → `AppState::Degraded`
//! - 不涉及对话管线、检索器、隐私确认、会话生命周期

use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{AppState, BackendConfig};
use ramaria_llm::keychain::Keychain;

use crate::App;

impl App {
    // =========================================================
    // 状态读写
    // =========================================================

    /// 获取当前应用状态。
    pub fn current_state(&self) -> AppState {
        *self.state.lock().unwrap_or_else(|e| {
            tracing::error!("App state lock poisoned: {e}");
            e.into_inner()
        })
    }

    /// 设置应用状态。
    ///
    /// 参数:
    /// - `new_state`: 目标状态。
    ///
    /// 说明:
    /// - 状态变更会记录 info 日志，便于诊断。
    pub fn set_state(&self, new_state: AppState) {
        let old = {
            let mut guard = self.state.lock().unwrap_or_else(|e| {
                tracing::error!("App state lock poisoned during set_state: {e}");
                e.into_inner()
            });
            let old = *guard;
            *guard = new_state;
            old
        };
        if old != new_state {
            tracing::info!(from = %old, to = %new_state, "App 状态变更");
        }
    }

    // =========================================================
    // LLM provider
    // =========================================================

    /// 获取后端配置引用。
    pub fn backend_config(&self) -> BackendConfig {
        self.llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone()
    }

    /// 热更新 LLM provider（配置修改后调用，替换内存中的 provider 实例）。
    pub fn update_llm(&self, new_llm: Arc<dyn LlmProvider>) {
        let mut guard = self.llm.lock().unwrap_or_else(|e| e.into_inner());
        tracing::info!(
            old_provider = guard.name(),
            new_provider = %new_llm.name(),
            "LLM provider 热更新"
        );
        *guard = new_llm;
    }

    /// 克隆当前 LLM provider 的 Arc（用于在锁外调用异步方法）。
    ///
    /// 返回:
    /// - 当前 LLM provider 的 `Arc<dyn LlmProvider>` 克隆。
    pub fn llm_clone(&self) -> Arc<dyn LlmProvider> {
        self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    // =========================================================
    // Keychain
    // =========================================================

    /// 获取 keychain 引用。
    pub fn keychain(&self) -> &Keychain {
        &self.keychain
    }

    /// 获取 keychain Arc 引用（供 provider 构造使用）。
    pub fn keychain_arc(&self) -> Arc<Keychain> {
        Arc::clone(&self.keychain)
    }

    // =========================================================
    // Embedding provider
    // =========================================================

    /// 获取当前嵌入模型 provider 的克隆。
    ///
    /// 返回:
    /// - `Some(Arc<dyn EmbeddingProvider>)`: 嵌入模型已配置且可用。
    /// - `None`: 未配置或不可用。
    pub fn embedding_provider(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 检查嵌入模型是否可用。
    ///
    /// 返回:
    /// - `true`: 嵌入模型已配置且 `is_available` 返回 true。
    pub fn is_embedding_available(&self) -> bool {
        self.embedding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|e| e.is_available())
            .unwrap_or(false)
    }

    /// 热更新嵌入模型 provider。
    ///
    /// 参数:
    /// - `new_embedding`: 新的嵌入 provider（Some 或 None 表示卸载）。
    pub fn update_embedding(&self, new_embedding: Option<Arc<dyn EmbeddingProvider>>) {
        let mut guard = self.embedding.lock().unwrap_or_else(|e| e.into_inner());
        match &new_embedding {
            Some(e) => tracing::info!(
                model = %e.model_info().model_id,
                dim = e.model_info().dimension,
                "嵌入模型热更新"
            ),
            None => tracing::info!("嵌入模型已卸载"),
        }
        *guard = new_embedding;
    }

    /// 尝试加载嵌入模型并更新应用状态。
    ///
    /// 说明:
    /// - 如果嵌入模型可用：状态保持不变（Ready 或继续 setup 流程）。
    /// - 如果嵌入模型缺失或不可用：进入 Degraded 状态，BM25+图谱仍可用。
    /// - 仅在 Ready 状态下调用此方法（索引构建完成后）。
    ///
    /// 返回:
    /// - `Ok(true)`: 嵌入模型可用，向量通道就绪。
    /// - `Ok(false)`: 嵌入模型不可用，已进入 Degraded。
    pub async fn try_load_embedding(&self) -> RamariaResult<bool> {
        let emb = {
            let guard = self.embedding.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };

        match emb {
            Some(ref provider) if provider.is_available() => match provider.validate().await {
                Ok(()) => {
                    tracing::info!(
                        model = %provider.model_info().model_id,
                        dim = provider.model_info().dimension,
                        "嵌入模型验证通过，向量通道可用"
                    );
                    Ok(true)
                }
                Err(e) => {
                    tracing::warn!(%e, "嵌入模型验证失败，进入降级模式");
                    self.set_state(AppState::Degraded);
                    Ok(false)
                }
            },
            _ => {
                tracing::info!("嵌入模型未配置，进入降级模式（BM25 + 图谱可用）");
                self.set_state(AppState::Degraded);
                Ok(false)
            }
        }
    }

    // =========================================================
    // 只读访问器
    // =========================================================

    /// 获取存储后端引用。
    ///
    /// 职责:
    /// - 供 CLI 等上层模块直接查询 sessions / memories / events 等数据。
    /// - 所有业务写操作应通过 App 方法执行，读操作可直接使用此引用。
    pub fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    /// 返回应用配置的只读引用。
    ///
    /// 用途:
    /// - 诊断导出需要读取日志目录和配置目录路径。
    /// - 外部模块需要读取配置参数。
    pub fn config(&self) -> &ramaria_core::config::RamariaConfig {
        &self.config
    }
}
