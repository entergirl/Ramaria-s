//! crates/ramaria-service/src/engine/usecases_channel.rs - Ramaria 社交通道用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在 `channel` 域模块，本层只做入口挂载
//! - 读用例实时解析：绑定解析（含自动建档）与生效人格按调用即时计算
//! - 凭据用例同步：keychain API 为同步调用，不做 async 包装

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{ChannelBinding, ChannelPlatform};

use super::Engine;

// =========================================================
// 通道绑定用例
// =========================================================

impl Engine {
    /// 通道绑定列表（只读，按 channel / external_ref 排序）。
    ///
    /// 返回:
    /// - 绑定行列表；查询失败上抛。
    pub async fn channel_list_bindings(&self) -> RamariaResult<Vec<ChannelBinding>> {
        crate::channel::list_bindings(self).await
    }

    /// 按 `(channel, external_ref)` 解析绑定；不存在时自动建档。
    ///
    /// 返回:
    /// - 命中的绑定行（含本次自动建档的行）。
    pub async fn channel_resolve_binding(
        &self,
        channel: &str,
        external_ref: &str,
    ) -> RamariaResult<ChannelBinding> {
        crate::channel::resolve_binding(self, channel, external_ref).await
    }

    /// 绑定写用例：更新绑定（不存在则先自动建档）。
    ///
    /// 参数:
    /// - `persona_uid`: `Some(v)` 校验后设置；`None` = 保持现值。
    /// - `enabled` / `allow_proactive`: `None` = 保持现值。
    ///
    /// 返回:
    /// - 校验失败为业务校验错误；查询 / 写入失败为存储错误。
    pub async fn channel_set_binding(
        &self,
        channel: &str,
        external_ref: &str,
        persona_uid: Option<&str>,
        enabled: Option<bool>,
        allow_proactive: Option<bool>,
    ) -> RamariaResult<ChannelBinding> {
        crate::channel::set_binding(
            self,
            channel,
            external_ref,
            persona_uid,
            enabled,
            allow_proactive,
        )
        .await
    }

    /// 删除通道绑定（幂等：不存在视同成功）。
    pub async fn channel_delete_binding(
        &self,
        channel: &str,
        external_ref: &str,
    ) -> RamariaResult<()> {
        crate::channel::delete_binding(self, channel, external_ref).await
    }

    /// 绑定生效人格：绑定级覆盖优先，其次通道级默认，再兜底默认人格。
    ///
    /// 返回:
    /// - 生效的本机侧人格标识（trim 后非空；纯读，不访问存储）。
    pub fn channel_effective_persona_uid(&self, binding: &ChannelBinding) -> String {
        let config = self.config();
        crate::channel::effective_persona_uid(&config.channel, binding)
    }

    // =========================================================
    // 平台凭据用例（OS keychain）
    // =========================================================

    /// 保存平台凭据（令牌写入 keychain；空白令牌为校验错误）。
    ///
    /// 返回:
    /// - 写入失败为隐私域错误（keychain 不可用 / 权限不足）。
    pub fn channel_save_credential(
        &self,
        platform: ChannelPlatform,
        token: &str,
    ) -> RamariaResult<()> {
        crate::channel::save_credential(self, platform, token)
    }

    /// 查询平台凭据是否已配置。
    pub fn channel_credential_configured(&self, platform: ChannelPlatform) -> RamariaResult<bool> {
        crate::channel::credential_configured(self, platform)
    }

    /// 读取平台凭据（None = 未配置）。
    pub fn channel_load_credential(
        &self,
        platform: ChannelPlatform,
    ) -> RamariaResult<Option<String>> {
        crate::channel::load_credential(self, platform)
    }
}
