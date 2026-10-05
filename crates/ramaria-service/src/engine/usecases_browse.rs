//! crates/ramaria-service/src/engine/usecases_browse.rs - Ramaria 浏览域用例挂载
//!
//! 设计特点:
//! - 以只读用例为主（会话未读标记为唯一写操作）：实现体在 `browse` 模块，本层只做入口挂载
//! - 记忆浏览：L1（会话口径 / persona 未吸收口径）、L2 事件、L3 标签与三层画像
//! - 知识浏览：活跃事实分页、单条事实版本链、按字段分组与版本链折叠
//! - 会话浏览：列表聚合与分页、消息正序 / 分页翻正、详情与计数、未读标记与汇总
//! - 证据链：trait → 证据记录 → 事件 → L1 溯源 → 证据片段
//! - 空态语义：不存在 / 无数据返回空列表或空链（非错误），由用例实现保证

use ramaria_core::error::RamariaResult;
use uuid::Uuid;

use crate::types::{
    ChannelOverviewView, FactBrowsePage, FactBrowseRequest, FactDetailView, GroupedFactsView,
    L1BrowsePage, L1BrowseRequest, L1MemoryView, L2BrowsePage, L2BrowseRequest, L3TraitView,
    PersonalityProfileView, ProfileStatusView, SessionBrowsePage, SessionBrowseRequest,
    SessionDetailView, SessionMessagesRequest, SessionMessagesView, TraitEvidenceRequest,
    TraitEvidenceView,
};

use super::Engine;

// =========================================================
// 记忆与会话浏览用例
// =========================================================

impl Engine {
    /// L1 记忆浏览用例：按会话收集摘要（桌面口径）或按 persona 取未吸收摘要（CLI 口径）。
    ///
    /// 返回:
    /// - `items`（分页后的摘要视图）与 `total`（排序后、分页前的条数）。
    pub async fn memory_l1(&self, req: L1BrowseRequest) -> RamariaResult<L1BrowsePage> {
        crate::browse::l1(self, req).await
    }

    /// L1 摘要按会话读取用例（封存结果的核对口径）。
    ///
    /// 返回:
    /// - 目标会话的全部摘要视图；会话不存在或无摘要均返回空列表（不报错）。
    pub async fn memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<Vec<L1MemoryView>> {
        crate::browse::l1_by_session(self, session_id).await
    }

    /// L2 事件浏览用例：persona 过滤分页（分页前总数）或全人格合并后统一排序截断。
    ///
    /// 返回:
    /// - `items` 与 `total`（分页前的条数）。
    pub async fn memory_l2(&self, req: L2BrowseRequest) -> RamariaResult<L2BrowsePage> {
        crate::browse::l2(self, req).await
    }

    /// L3 性格标签浏览用例（扁平列表；persona 缺省时合并全部人格）。
    pub async fn memory_l3(&self, persona: Option<&str>) -> RamariaResult<Vec<L3TraitView>> {
        crate::browse::l3(self, persona).await
    }

    /// L3 三层性格画像用例（base / primary / accent 分组，仅生效标签；人格不存在报错）。
    pub async fn personality_profile(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<PersonalityProfileView> {
        crate::browse::personality_profile(self, persona_uid).await
    }

    /// 画像数据状态用例（有效样本量与可信度区间：insufficient / preliminary / trusted）。
    pub async fn profile_status(&self, persona_uid: &str) -> RamariaResult<ProfileStatusView> {
        crate::browse::profile_status(self, persona_uid).await
    }

    /// 性格标签证据链用例：trait → 证据记录 → 事件 → L1 溯源 → 证据片段。
    ///
    /// 返回:
    /// - 单元素列表（证据链视图）；无证据记录时返回单条空链（非错误）。
    pub async fn memory_trait_evidence(
        &self,
        req: TraitEvidenceRequest,
    ) -> RamariaResult<Vec<TraitEvidenceView>> {
        crate::browse::trait_evidence(self, req).await
    }

    /// 知识事实浏览用例：活跃事实，可选按字段过滤后分页。
    ///
    /// 返回:
    /// - `items`（全字段视图）与 `total`（分页前的条数）。
    pub async fn memory_facts(&self, req: FactBrowseRequest) -> RamariaResult<FactBrowsePage> {
        crate::browse::facts(self, req).await
    }

    /// 单条事实详情用例：含完整版本链（链头最早在前）；不存在时返回 `None`。
    pub async fn memory_fact_detail(&self, id: i64) -> RamariaResult<Option<FactDetailView>> {
        crate::browse::fact_detail(self, id).await
    }

    /// 知识事实分组用例：按字段分组 + 多版本事实的版本链折叠数据。
    pub async fn memory_facts_grouped(&self, persona: &str) -> RamariaResult<GroupedFactsView> {
        crate::browse::facts_grouped(self, persona).await
    }

    /// 会话列表浏览用例：开始时间倒序 + 消息计数聚合 + 分页（`limit` 缺省返回全部）。
    pub async fn session_list(
        &self,
        req: SessionBrowseRequest,
    ) -> RamariaResult<SessionBrowsePage> {
        crate::browse::sessions(self, req).await
    }

    /// 会话消息浏览用例：全量正序（`limit` 为 None）或最新在前分页后翻正。
    pub async fn session_messages(
        &self,
        req: SessionMessagesRequest,
    ) -> RamariaResult<SessionMessagesView> {
        crate::browse::session_messages(self, req).await
    }

    /// 会话详情用例：会话元数据 + 消息页（全量或分页后翻正）。
    ///
    /// 返回:
    /// - 会话不存在时返回 `Validation` 错误。
    pub async fn session_detail(
        &self,
        session_id: Uuid,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> RamariaResult<SessionDetailView> {
        crate::browse::session_detail(self, session_id, limit, offset).await
    }

    /// 会话消息计数用例（诊断用；查询失败按 0 处理，不阻塞主流程）。
    pub async fn count_session_messages(&self, session_id: Uuid) -> usize {
        crate::browse::count_session_messages(self, session_id).await
    }

    /// 标记会话已读用例：推进已读时间到当前（幂等；会话不存在也视为成功）。
    pub async fn session_mark_read(&self, session_id: Uuid) -> RamariaResult<()> {
        crate::browse::mark_session_read(self, session_id).await
    }

    /// 全部会话未读总数用例（托盘徽标与全局未读提示口径）。
    pub async fn unread_total(&self) -> RamariaResult<u32> {
        crate::browse::unread_total(self).await
    }

    /// 通道会话概览用例：该通道的活跃会话数与最近活动时间（只读聚合）。
    pub async fn channel_overview(&self, channel: &str) -> RamariaResult<ChannelOverviewView> {
        crate::browse::channel_overview(self, channel).await
    }
}
