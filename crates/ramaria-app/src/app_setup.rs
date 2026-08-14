//! crates/ramaria-app/src/app_setup.rs - 设置流程代理方法
//!
//! 设计特点:
//! - 从 `app.rs` 拆分，减少 App 本体的行数
//! - `run_setup`: 保存后端配置 → 健康探测 → 更新状态
//! - `probe_health_with_retry`: LLM 后端健康检查（最多 3 次重试，间隔 2s）
//! - `refresh_setup_state`: 检查嵌入模型状态并刷新应用状态

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::LlmProvider;
use ramaria_core::types::{AppState, BackendConfig};

use super::App;

impl App {
    /// 执行设置流程：保存后端配置 → 健康探测 → 更新状态。
    ///
    /// 参数:
    /// - `backend_config`: 用户选择的后端配置。
    ///
    /// 返回:
    /// - 设置后的最终状态（健康检查失败时置为 Degraded，但不会返回 Err）。
    ///
    /// 说明:
    /// - 健康检查超时 5 秒，不阻塞主流程。
    /// - 健康检查失败仅降级（Degraded 状态），不阻止后续操作。
    pub async fn run_setup(&self, backend_config: &BackendConfig) -> RamariaResult<AppState> {
        // Step 1: 保存后端配置、检查索引状态
        let state = crate::setup::run_setup(self.storage.as_ref(), backend_config).await?;

        // Step 2: LLM 后端健康探测（最多 3 次，间隔 2s）
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let health_ok = Self::probe_health_with_retry(llm.as_ref(), 3, 2).await;

        let final_state = if !health_ok {
            tracing::warn!(
                provider = %backend_config.provider,
                base_url = %backend_config.base_url,
                "LLM 后端健康检查失败，应用将进入 Degraded 状态"
            );
            // 健康检查失败 → Degraded（非阻塞，BM25+图谱仍可用）
            AppState::Degraded
        } else {
            tracing::info!(
                provider = %backend_config.provider,
                "LLM 后端健康检查通过"
            );
            state
        };

        self.set_state(final_state);
        Ok(final_state)
    }

    /// 带重试的健康探测。
    ///
    /// 参数:
    /// - `llm`: LLM provider 引用。
    /// - `max_retries`: 最多重试次数（默认 3）。
    /// - `interval_secs`: 每次间隔秒数（默认 2）。
    ///
    /// 返回:
    /// - `true`: 健康检查通过（至少一次成功）。
    /// - `false`: 所有重试均失败。
    async fn probe_health_with_retry(
        llm: &dyn LlmProvider,
        max_retries: u32,
        interval_secs: u64,
    ) -> bool {
        for attempt in 0..max_retries {
            match llm.health_check().await {
                Ok(()) => return true,
                Err(e) => {
                    if attempt + 1 < max_retries {
                        tracing::warn!(
                            attempt = attempt + 1,
                            max_retries,
                            error = %e,
                            "健康检查失败，即将重试"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                    } else {
                        tracing::error!(
                            attempt = attempt + 1,
                            max_retries,
                            error = %e,
                            "健康检查全部失败"
                        );
                    }
                }
            }
        }
        false
    }

    /// 检查并更新设置状态（含嵌入模型状态检查）。
    pub async fn refresh_setup_state(&self) -> RamariaResult<AppState> {
        let embedding_available = self.is_embedding_available();
        let status =
            crate::setup::check_setup_status(self.storage.as_ref(), embedding_available).await?;
        let state = crate::setup::determine_state(&status);
        self.set_state(state);
        Ok(state)
    }
}
