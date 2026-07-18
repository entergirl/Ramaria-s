//! rust/crates/ramaria-app/src/app_privacy.rs - 隐私确认代理方法
//!
//! 设计特点:
//! - 从 `app.rs` 拆分，减少 App 本体的行数（M-5 修复）
//! - `check_privacy`: 查询当前 provider 的隐私确认状态
//! - `confirm_privacy`: 记录隐私确认（支持跨重启持久化）
//! - 委托 `crate::privacy` 模块执行具体逻辑

use ramaria_core::error::RamariaResult;

use super::App;

impl App {
    /// 检查当前 provider 的隐私确认状态。
    pub async fn check_privacy(&self) -> RamariaResult<crate::privacy::PrivacyStatus> {
        let cfg = self
            .llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone();
        crate::privacy::check_privacy(self.storage.as_ref(), cfg.provider, &cfg.base_url).await
    }

    /// 记录隐私确认。
    ///
    /// 参数:
    /// - `persistent`: 是否跨重启持久化。
    pub async fn confirm_privacy(&self, persistent: bool) -> RamariaResult<()> {
        let cfg = self
            .llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone();
        crate::privacy::confirm_privacy(
            self.storage.as_ref(),
            cfg.provider,
            &cfg.base_url,
            persistent,
        )
        .await
    }
}
