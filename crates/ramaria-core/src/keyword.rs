//! crates/ramaria-core/src/keyword.rs - Ramaria 关键词类型系统
//!
//! 设计特点:
//! - `KeywordToken`: 标准化关键词 Newtype，自动 trim + 小写 + 非空校验
//! - `KeywordSet`: 保留插入顺序的去重集合，驱动 TopicBatcher 关键词图构建
//! - `KeywordStatus`: 三态枚举（Canonical / Alias / Pending），支撑别名归一化管线
//! - `KeywordRef`: 倒排索引引用枚举（L1/L2/Pool），关联关键词与业务文档
//! - `KeywordQuery`: 类型安全检索查询参数（关键词集 + persona + 匹配策略 + top_k）
//! - `MatchStrategy`: 字面匹配策略（Exact / Substring，按设计去除 Prefix）
//! - 纯类型层，零 I/O，零外部依赖（仅 serde + uuid），完全符合 ramaria-core 零 I/O 约束
//! - M3（T-V20-3-001）起 KeywordQuery/MatchStrategy 成为 keyword/index.rs、
//!   keyword/composite.rs 的查询入口类型；KeywordRef 作为检索结果的文档标识返回

use serde::{Deserialize, Serialize};

mod rows;
mod token;

pub use rows::{KeywordPoolRow, KeywordRef, KeywordStatus, MatchStrategy, PendingAliasRow};
pub use token::{KeywordSet, KeywordToken};

// =========================================================
// KeywordQuery — 类型安全的检索查询参数
// =========================================================

/// 关键词检索查询——携带关键词集合、persona 隔离键、匹配策略与返回上限。
///
/// # 字段语义
///
/// - `keywords`: 待检索的标准关键词集合（应为已标准化 token）。
/// - `persona_uid`: 文档归属隔离键；`None` 表示不过滤 persona（仅在明确需要
///   全局检索时使用，默认应显式提供 persona）。
/// - `top_k`: 最大返回条数，构造时钳制在 `1..=MAX_TOP_K`。
/// - `strategy`: 字面匹配策略（Exact / Substring）。
///
/// # 构造约定
///
/// 通过 `KeywordQuery::builder(persona_uid)` 构建，保证 `top_k` 恒落在合法区间，
/// 避免调用方传入 `0` 或超大值导致边界问题。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordQuery {
    /// 待检索的关键词集合
    pub keywords: KeywordSet,
    /// 文档归属 persona 隔离键（None = 不过滤）
    pub persona_uid: Option<String>,
    /// 最大返回条数（1..=MAX_TOP_K，构造时钳制）
    pub top_k: usize,
    /// 字面匹配策略
    pub strategy: MatchStrategy,
}

/// KeywordQuery.top_k 的最大合法值（上限 100，防止单次检索返回过大集合）。
pub const MAX_QUERY_TOP_K: usize = 100;

/// KeywordQuery 默认 top_k。
const DEFAULT_TOP_K: usize = 10;

impl KeywordQuery {
    /// 创建查询构建器。
    ///
    /// 参数:
    /// - `persona_uid`: 文档归属 persona（`None` 表示全局不过滤，需调用方显式确认）。
    ///
    /// 返回:
    /// - 以默认参数（Exact / top_k=10）为起点的构建器。
    pub fn builder(persona_uid: Option<String>) -> KeywordQueryBuilder {
        KeywordQueryBuilder {
            keywords: KeywordSet::new(),
            persona_uid,
            top_k: DEFAULT_TOP_K,
            strategy: MatchStrategy::Exact,
        }
    }

    /// 校验并钳制查询参数（内部构造完成后调用，保证不变量）。
    fn sanitize(&mut self) {
        self.top_k = self.top_k.clamp(1, MAX_QUERY_TOP_K);
    }
}

/// `KeywordQuery` 类型安全构建器。
///
/// 用法:
/// ```
/// use ramaria_core::keyword::{KeywordQuery, KeywordSet, KeywordToken, MatchStrategy};
/// let mut set = KeywordSet::new();
/// set.insert(KeywordToken::new("工作压力").unwrap());
/// let q = KeywordQuery::builder(Some("user-0001".into()))
///     .with_keywords(set)
///     .top_k(5)
///     .strategy(MatchStrategy::Substring)
///     .build();
/// assert_eq!(q.top_k, 5);
/// ```
#[derive(Debug, Clone)]
pub struct KeywordQueryBuilder {
    keywords: KeywordSet,
    persona_uid: Option<String>,
    top_k: usize,
    strategy: MatchStrategy,
}

impl KeywordQueryBuilder {
    /// 设置查询关键词集合。
    pub fn with_keywords(mut self, keywords: KeywordSet) -> Self {
        self.keywords = keywords;
        self
    }

    /// 从 token 迭代器设置查询关键词集合。
    pub fn keywords_from<I: IntoIterator<Item = KeywordToken>>(mut self, iter: I) -> Self {
        self.keywords = iter.into_iter().collect();
        self
    }

    /// 追加单个查询关键词。
    pub fn add_keyword(mut self, token: KeywordToken) -> Self {
        self.keywords.insert(token);
        self
    }

    /// 设置最大返回条数（构造时钳制到 1..=100）。
    pub fn top_k(mut self, k: usize) -> Self {
        self.top_k = k.clamp(1, MAX_QUERY_TOP_K);
        self
    }

    /// 设置字面匹配策略。
    pub fn strategy(mut self, s: MatchStrategy) -> Self {
        self.strategy = s;
        self
    }

    /// 构建最终查询。
    pub fn build(self) -> KeywordQuery {
        let mut q = KeywordQuery {
            keywords: self.keywords,
            persona_uid: self.persona_uid,
            top_k: self.top_k,
            strategy: self.strategy,
        };
        q.sanitize();
        q
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
