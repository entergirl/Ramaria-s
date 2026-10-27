//! crates/ramaria-service/src/engine/usecases_channel.rs - Ramaria 社交通道用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在 `channel` 域模块，本层只做入口挂载
//! - 读用例实时解析：绑定解析（含自动建档）/ 只读查询与生效人格按调用即时计算
//! - 凭据用例同步：keychain API 为同步调用，不做 async 包装
//! - 入站写入 / 回复生成 / 图片理解 / 撤回通知 / 表情解析用例供通道运行时编排消费

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{ChannelBinding, ChannelPlatform, now_ms};

use crate::types::{
    ChannelImageOutcome, ChannelIngestOutcome, ChannelIngestRequest, ChannelReplyRequest,
    ChatSendOutcome, RecallNoticeRequest, RecallOutcome, StickerPayload,
};

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

    /// 只读查询绑定（不自动建档）；不存在返回 None。
    ///
    /// 说明:
    /// - 供投递门禁等"绑定不存在即不投递"的消费方使用（不产生副作用）。
    pub async fn channel_find_binding(
        &self,
        channel: &str,
        external_ref: &str,
    ) -> RamariaResult<Option<ChannelBinding>> {
        crate::channel::find_binding(self, channel, external_ref).await
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

    /// 绑定生效人格：绑定级覆盖优先，其次通道级默认，再兜底默认对话人格。
    ///
    /// 返回:
    /// - 生效的本机侧人格标识（trim 后非空）；
    /// - 默认对话人格取 `settings.default_persona_uid`（不可用时回退内置默认）。
    pub async fn channel_effective_persona_uid(&self, binding: &ChannelBinding) -> String {
        let config = self.config();
        let fallback = self.default_persona_uid().await;
        crate::channel::effective_persona_uid(&config.channel, binding, &fallback)
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

    /// 清除平台凭据（幂等；运行中的通道需重新启停才按清除后的状态连接）。
    pub fn channel_clear_credential(&self, platform: ChannelPlatform) -> RamariaResult<()> {
        crate::channel::clear_credential(self, platform)
    }

    // =========================================================
    // 通道消息用例（入站写入 / 回复生成 / 图片理解）
    // =========================================================

    /// 社交通道入站写入：会话解析 / 发言人归属 / 附件行 / 成员 upsert（幂等）。
    ///
    /// 返回:
    /// - `session_id` / `written` / 图片附件定位（`written=false` 为重复事件）。
    pub async fn channel_ingest(
        &self,
        req: ChannelIngestRequest,
    ) -> RamariaResult<ChannelIngestOutcome> {
        crate::channel::ingest::run(self, req).await
    }

    /// 社交通道回复生成：入站消息已落库，只写助手回复行。
    ///
    /// 返回:
    /// - `Some(outcome)`: `reply` / `session_id` / `chars`（空回复不落库，
    ///   由调用方跳过发送）；
    /// - `None`: 触发消息已撤回，回复作废（不落库不发送）。
    pub async fn channel_reply(
        &self,
        req: ChannelReplyRequest,
    ) -> RamariaResult<Option<ChatSendOutcome>> {
        crate::chat::reply_to_inbound(self, req).await
    }

    /// 处理一条通道消息撤回通知（标记 + 触发展开回应）。
    ///
    /// 返回:
    /// - 处理结果三态（已标记 / 忽略 / 重复通知）。
    pub async fn channel_recall(
        &self,
        notice: RecallNoticeRequest,
    ) -> RamariaResult<RecallOutcome> {
        crate::withdraw::handle_notice(self, notice, now_ms()).await
    }

    /// 解析表情主题（匹配资产并读取文件字节）。
    ///
    /// 返回:
    /// - 命中返回资产载荷；未命中 / 文件读取失败返回 None（降级不阻塞发送）。
    pub async fn sticker_resolve(&self, keyword: &str) -> RamariaResult<Option<StickerPayload>> {
        crate::sticker::resolve(self, keyword).await
    }

    /// 理解一张已下载图片（字节输入；门禁 / 去重 / 回填与批量路径同口径）。
    ///
    /// 返回:
    /// - 单张理解结果（统计口径）。
    pub async fn channel_understand_image(
        &self,
        attachment_id: i64,
        md5: &str,
        image_bytes: &[u8],
    ) -> RamariaResult<ChannelImageOutcome> {
        crate::vision::understand_downloaded_image(self, attachment_id, md5, image_bytes).await
    }
}
