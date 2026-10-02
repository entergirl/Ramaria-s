//! crates/ramaria-service/src/types/facts.rs - Ramaria 知识事实浏览用例数据结构
//!
//! 设计特点:
//! - 覆盖事实条目 / 分组 / 版本链与详情浏览
//! - 字段与来源枚举沿用内核类型，序列化口径与其一致
//! - 版本链折叠数据以 HashMap 承载（仅多版本事实入表）
//! - 内容为陈述句，非原文

use std::collections::HashMap;

use ramaria_core::types::{FactSource, FactStatus, FactTier, ProfileField};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =========================================================
// 知识事实浏览用例（条目 / 分组 / 版本链）
// =========================================================

/// 知识事实浏览请求。
///
/// 字段约定:
/// - `persona`: 目标人格 uid（必填）。
/// - `field`: 可选字段过滤（None = 全部字段）。
/// - `limit`: 返回条数上限（None = 全部；默认值由调用点决定）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactBrowseRequest {
    pub persona: String,
    pub field: Option<ProfileField>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 知识事实条目视图（全字段；内容为陈述句，非原文）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEntryView {
    /// 事实 id
    pub id: i64,
    /// 字段归属人格
    pub persona_uid: String,
    pub field: ProfileField,
    pub content: String,
    /// 来源（event / manual / l1）
    pub source: FactSource,
    /// 生命周期状态（active / superseded / candidate）
    pub status: FactStatus,
    /// 分层（stable / volatile / historical）
    pub tier: FactTier,
    /// 覆盖链：被替换事实 id（沿此可展开历史版本）
    pub version_of: Option<i64>,
    pub confidence: f64,
    /// 关键词（判重 / 检索提示）
    pub keyword_hint: Option<String>,
    /// 来源事件 id
    pub ref_event_id: Option<i64>,
    /// 来源 L1 id
    pub ref_l1_id: Option<Uuid>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 知识事实浏览响应。
///
/// 字段约定:
/// - `total`: 分页前的条数（调用方据此判断是否还有下一页）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactBrowsePage {
    pub items: Vec<FactEntryView>,
    pub total: usize,
}

/// 单条事实详情（含完整版本链）。
///
/// 字段约定:
/// - `versions`: 含自身的完整版本链（链头最早在前）；单版本事实仅含自身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactDetailView {
    pub fact: FactEntryView,
    pub versions: Vec<FactEntryView>,
}

/// 按字段分组的知识事实视图（含版本链折叠数据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedFactsView {
    pub persona_uid: String,
    /// 按字段展示名分组: { field_label: [活跃事实] }
    pub grouped: HashMap<String, Vec<FactEntryView>>,
    /// 版本链查找: { fact_id: [旧→新版本链] }（仅多版本事实入表）
    pub versions: HashMap<i64, Vec<FactEntryView>>,
}
