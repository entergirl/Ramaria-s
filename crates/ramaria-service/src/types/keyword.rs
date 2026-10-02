//! crates/ramaria-service/src/types/keyword.rs - Ramaria 关键词用例数据结构
//!
//! 设计特点:
//! - 覆盖关键词池 / 待确认别名 / 裁决 / seed / 建议五类用例
//! - 状态以三态字符串（canonical / alias / pending）对外表达
//! - 裁决动作与幂等处置（already_applied_ok）由请求字段显式给定
//! - 计数口径与入口展示一致（seeded + skipped 恒等于结果条数）

use serde::{Deserialize, Serialize};

// =========================================================
// 关键词用例（关键词池列表 / 待确认别名 / 别名裁决）
// =========================================================

/// 关键词池词条视图。
///
/// 字段约定:
/// - `status`: 三态字符串（canonical / alias / pending）。
/// - `canonical_id` / `canonical_keyword`: 指向的规范词（规范词自身为 None）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordEntryView {
    pub keyword: String,
    /// 使用次数（自然出现 +1；手工种子为 0）
    pub use_count: i64,
    pub status: String,
    pub canonical_id: Option<i64>,
    pub canonical_keyword: Option<String>,
    pub created_at: i64,
}

/// 关键词池列表视图（三态计数 + 全量词条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordPoolView {
    pub total: usize,
    pub canonical_count: usize,
    pub alias_count: usize,
    pub pending_count: usize,
    pub keywords: Vec<KeywordEntryView>,
}

/// 待确认别名视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAliasView {
    /// 别名词条 rowid（裁决时定位行）
    pub alias_id: i64,
    /// 别名文本
    pub alias: String,
    /// 建议合并到的规范词文本
    pub canonical: String,
    pub created_at: i64,
}

/// 别名裁决动作。
///
/// 格式:
/// - 序列化为小写字符串：`confirm` / `reject`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasAction {
    /// 确认合并（pending → alias）
    Confirm,
    /// 驳回晋升（pending → canonical）
    Reject,
}

/// 别名裁决请求。
///
/// 字段约定:
/// - `alias`: 待处理的别名文本（标准化后比较）。
/// - `action`: 裁决动作（确认 / 驳回）。
/// - `already_applied_ok`: confirm 且词条已是 alias 时的处置——true = 幂等成功
///   （不写库，`already_applied` 置位）；false = 报业务校验错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AliasResolveRequest {
    pub alias: String,
    pub action: AliasAction,
    pub already_applied_ok: bool,
}

/// 别名裁决结果。
///
/// 字段约定:
/// - `alias`: 标准化后的别名文本。
/// - `canonical_keyword`: confirm 后指向的规范词文本（reject 后为 None）。
/// - `status`: 裁决后的状态（`alias` / `canonical`）。
/// - `already_applied`: true = 本次未写库（目标状态此前已达成，幂等路径）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AliasResolveOutcome {
    pub alias: String,
    pub canonical_keyword: Option<String>,
    pub status: String,
    pub already_applied: bool,
}

/// 关键词 seed 单条结果。
///
/// 字段约定:
/// - `inserted`: true = 本次新插入（use_count 从 0 起）；false = 词条已存在（保持现状）。
/// - `status`: 处理后的词条状态（canonical / alias / pending）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSeedItem {
    pub keyword: String,
    pub inserted: bool,
    pub status: String,
}

/// 关键词 seed 结果。
///
/// 字段约定:
/// - `seeded` / `skipped`: 新插入 / 已存在跳过的条数之和恒等于 `results.len()`；
/// - `results`: 去重后的逐条结果（保留首次出现顺序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSeedOutcome {
    pub seeded: usize,
    pub skipped: usize,
    pub results: Vec<KeywordSeedItem>,
}

/// 关键词别名建议结果。
///
/// 字段约定:
/// - `scanned_tokens`: 参与分析的词条数（关键词池行与内存镜像的使用量按文本合并后）；
/// - `suggestions`: 相似度引擎产出的原始建议数（尚未过滤）；
/// - `inserted`: 本次新登记的待确认别名数；
/// - `skipped`: 因已存在词条 / 已建立状态 / 单条写入落败而跳过的建议数；
/// - `truncated`: 因单次运行登记上限而未处理的建议数；
/// - `message`: 面向入口的汇总提示（计数口径与上述字段一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordSuggestionOutcome {
    pub scanned_tokens: usize,
    pub suggestions: usize,
    pub inserted: usize,
    pub skipped: usize,
    pub truncated: usize,
    pub message: String,
}
