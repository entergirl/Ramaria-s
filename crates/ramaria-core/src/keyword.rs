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
use std::fmt;

// =========================================================
// KeywordToken — 标准化关键词 Newtype
// =========================================================

/// 已标准化关键词标记（Newtype）。
///
/// 职责:
/// - 替代裸 `String` 传递关键词，编译期确保关键词已通过标准化处理
/// - 自动 trim 前/后空白 + 英文字母小写 + 非空校验
/// - 提供 `as_str()` 零开销访问内部字符串
///
/// 格式:
/// - 英文部分统一小写（如 "Work" → "work"）
/// - 中文保持原样（如 "工作压力" 保持不变）
/// - 前后空白被 trim（如 "  工作压力  " → "工作压力"）
/// - 空字符串或纯空白无法构造（`new()` 返回 `None`）
///
/// 安全约束:
/// - `new()` 是唯一会执行完整标准化（trim + 小写 + 非空 + 长度）的构造入口。
/// - 存在两个不做重复校验的旁路：`from_validated`（调用方保证已标准化）与
///   derive 的 `Deserialize`（反序列化不校验不变量）；内部 String 不对外暴露可变引用，
///   `as_str()` 只读访问。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeywordToken(String);

impl KeywordToken {
    /// 从原始字符串创建 `KeywordToken`。
    ///
    /// 参数:
    /// - `raw`: 原始关键词字符串。
    ///
    /// 返回:
    /// - `Some(Self)`: 标准化后的关键词（trim + 英文小写 + 非空）。
    /// - `None`: 输入为空字符串、纯空白或长度超过 256 字符。
    ///
    /// 说明:
    /// - 英文小写：仅 ASCII 字母转为小写（不涉及 Unicode 大小写折叠）。
    /// - 中文/日文等非 ASCII 字符保持不变。
    /// - 最大长度 256 字符（UTF-8 字节数），防止异常长输入。
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        // 长度限制：最长 256 字符（UTF-8 字节数）
        if trimmed.len() > 256 {
            return None;
        }
        // ASCII 字母小写化（仅 a-z/A-Z，不影响中文）
        let normalized: String = trimmed
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c
                }
            })
            .collect();

        // 二次校验：小写化后可能变为空（纯标点符号场景极少，但防御）
        if normalized.trim().is_empty() {
            return None;
        }

        Some(Self(normalized))
    }

    /// 返回内部字符串引用，零开销。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费 self，返回内部字符串。
    pub fn into_inner(self) -> String {
        self.0
    }

    /// 返回字符串长度（UTF-8 字节数）。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 关键词是否为空（由构造保证永远不会为 true，保留用于泛型一致性）。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 从已校验的标准化字符串创建（跳过重复校验）。
    ///
    /// # Safety
    ///
    /// 调用方须保证 `s` 已满足不变量（非空、trim、ASCII 小写、含字母数字、≤256 字节）。
    /// 供内部确知已标准化的 token 复用（如 bigram/词典命中），避免二次构造开销。
    #[inline]
    pub fn from_validated(s: String) -> Self {
        debug_assert!(!s.is_empty(), "KeywordToken 不能为空");
        debug_assert!(
            s.chars().any(|c| c.is_alphanumeric()),
            "KeywordToken 需至少含一个字母数字字符: {s}"
        );
        Self(s)
    }
}

impl fmt::Display for KeywordToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<KeywordToken> for String {
    fn from(token: KeywordToken) -> Self {
        token.0
    }
}

impl AsRef<str> for KeywordToken {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for KeywordToken {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

// =========================================================
// KeywordSet — 保留插入顺序的去重集合
// =========================================================

/// 去重关键词集合（保留插入顺序）。
///
/// 职责:
/// - 替代 `Vec<KeywordToken>` 或 `HashSet<KeywordToken>`，兼顾去重和有序性
/// - 供 L1 摘要、事件提取、TopicBatcher 等场景使用
/// - 内部使用 `Vec<KeywordToken>` + `insert` 时线性去重（集合规模小，< 50 个）
///
/// 字段约定:
/// - `tokens`: 保留插入顺序的向量
/// - 插入时若 `tokens` 已包含相同 `KeywordToken`，跳过
///
/// 性能说明:
/// - 关键词集合通常 < 20 个，线性查找去重已足够
/// - 避免引入 `indexmap` 等外部依赖
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordSet {
    tokens: Vec<KeywordToken>,
}

impl KeywordSet {
    /// 创建空集合。
    pub fn new() -> Self {
        Self { tokens: Vec::new() }
    }

    /// 插入一个关键词（去重，保留插入顺序）。
    ///
    /// 返回:
    /// - `true`: 新插入（之前不存在）。
    /// - `false`: 已存在，未重复插入。
    pub fn insert(&mut self, token: KeywordToken) -> bool {
        if self.tokens.contains(&token) {
            false
        } else {
            self.tokens.push(token);
            true
        }
    }

    /// 返回集合中关键词数量。
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// 集合是否为空。
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// 返回关键词的只读迭代器。
    pub fn iter(&self) -> impl Iterator<Item = &KeywordToken> {
        self.tokens.iter()
    }

    /// 将关键词集合转换为 `Vec<String>`（标准化后的字符串）。
    pub fn into_strings(self) -> Vec<String> {
        self.tokens.into_iter().map(|t| t.into_inner()).collect()
    }

    /// 检查集合是否包含指定关键词。
    pub fn contains(&self, token: &KeywordToken) -> bool {
        self.tokens.contains(token)
    }

    /// 扩展集合（从迭代器批量插入）。
    pub fn extend<I: IntoIterator<Item = KeywordToken>>(&mut self, iter: I) {
        for token in iter {
            self.insert(token);
        }
    }
}

impl FromIterator<KeywordToken> for KeywordSet {
    fn from_iter<I: IntoIterator<Item = KeywordToken>>(iter: I) -> Self {
        let mut set = Self::new();
        for token in iter {
            set.insert(token);
        }
        set
    }
}

impl IntoIterator for KeywordSet {
    type Item = KeywordToken;
    type IntoIter = std::vec::IntoIter<KeywordToken>;

    fn into_iter(self) -> Self::IntoIter {
        self.tokens.into_iter()
    }
}

impl<'a> IntoIterator for &'a KeywordSet {
    type Item = &'a KeywordToken;
    type IntoIter = std::slice::Iter<'a, KeywordToken>;

    fn into_iter(self) -> Self::IntoIter {
        self.tokens.iter()
    }
}

impl Extend<KeywordToken> for KeywordSet {
    fn extend<I: IntoIterator<Item = KeywordToken>>(&mut self, iter: I) {
        self.extend(iter);
    }
}

// =========================================================
// KeywordStatus — 别名归一化三态枚举
// =========================================================

/// 关键词别名状态——标识一个关键词在 keyword_pool 中的角色。
///
/// 职责:
/// - 支撑别名归一化管线：区分规范词、别名和待审核别名
/// - 供 `keyword_pool.alias_status` 字段的类型安全映射
///
/// 状态说明:
/// - `Canonical`: 规范词（如 "工作压力"），所有别名指向此词
/// - `Alias { canonical_id }`: 已确认的别名，指向规范词（如 "职场焦虑" → "工作压力"）
/// - `Pending { suggested_canonical_id }`: 待审核别名，系统建议合并到此规范词
///
/// 使用约定:
/// - `canonical_id` 和 `suggested_canonical_id` 指向 `keyword_pool.id`（INTEGER 主键）
/// - `Pending` 状态的词在别名管理员确认后改为 `Alias`
/// - `Canonical` 状态的词可被指定为其他 Canonical 词的别名（发生合并时）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeywordStatus {
    /// 规范词——别名系统中的权威词条
    Canonical,
    /// 已确认别名，指向规范词
    Alias {
        /// 规范词在 keyword_pool 中的 id
        canonical_id: i64,
    },
    /// 待审核别名，系统建议合并到此规范词
    Pending {
        /// 建议的规范词在 keyword_pool 中的 id
        suggested_canonical_id: i64,
    },
}

impl KeywordStatus {
    /// 返回状态的简短字符串描述，用于日志和调试。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Alias { .. } => "alias",
            Self::Pending { .. } => "pending",
        }
    }

    /// 是否 Canonical 状态。
    pub fn is_canonical(&self) -> bool {
        matches!(self, Self::Canonical)
    }

    /// 尝试获取指向的规范词 ID。
    ///
    /// 返回:
    /// - `Some(i64)`: Alias 或 Pending 状态的 canonical_id / suggested_canonical_id。
    /// - `None`: Canonical 状态（自身即为规范词）。
    pub fn canonical_id(&self) -> Option<i64> {
        match self {
            Self::Canonical => None,
            Self::Alias { canonical_id } => Some(*canonical_id),
            Self::Pending {
                suggested_canonical_id,
            } => Some(*suggested_canonical_id),
        }
    }
}

impl Default for KeywordStatus {
    /// 默认状态为 `Canonical`。
    ///
    /// 说明:
    /// - 新创建的关键词在别名系统确认前默认为规范词
    /// - 后续通过别名管理模块将同义词标记为 Alias 或 Pending
    fn default() -> Self {
        Self::Canonical
    }
}

// =========================================================
// KeywordPoolRow — keyword_pool 词条装载行
// =========================================================

/// keyword_pool 词条装载行（KeywordService / KeywordPool 装载的最小数据形态）。
///
/// 职责:
/// - 供 `ramaria-storage` 返回 keyword_pool 全量词条、`ramaria-memory` 装载
///   `KeywordPool`（三态状态机）使用，零 I/O 纯数据行。
/// - `rowid` 与 keyword_pool 的 rowid（INTEGER 主键）一致——`KeywordStatus::Alias` /
///   `Pending` 的 `canonical_id` 即指向该行 id，装载时必须保留。
///
/// 字段约定:
/// - `alias_status`: `NULL` / `"canonical"` → 规范词；`"alias"` → 已确认别名；
///   `"pending"` → 待确认别名。
/// - `canonical_id`: 指向的规范词行 id（规范词自身为 `NULL`）。
/// - `canonical_keyword`: LEFT JOIN 解析出的规范词文本（展示用途；状态机装载不依赖）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordPoolRow {
    /// keyword_pool.rowid（INTEGER 自增 rowid）
    pub rowid: i64,
    /// 标准化关键词文本
    pub keyword: String,
    /// 使用次数（每次自然出现 +1；手工种子为 0）
    pub use_count: i64,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 别名状态文本（"canonical" / "alias" / "pending"，规范词可为 NULL）
    pub alias_status: Option<String>,
    /// 指向的规范词行 id（别名/待确认词条有值；规范词自身为 NULL）
    pub canonical_id: Option<i64>,
    /// 指向规范词的文本（LEFT JOIN 解析；规范词自身为 None）
    pub canonical_keyword: Option<String>,
}

// =========================================================
// KeywordRef — 倒排索引引用枚举
// =========================================================

/// 关键词倒排引用——标识一个关键词出现在哪些业务文档 / 词典词条中。
///
/// # 语义约定（M3 消费路径定稿）
///
/// - 作为 `ramaria-memory` KeywordIndex / CompositeIndex 检索结果的**文档标识**返回，
///   调用方依据 `doc_type()` + `doc_id_text()` 回查业务文档内容（对应
///   `keyword_refs` 表 `(doc_type, doc_id)` 的语义主键形态）。
/// - L1 摘要以 `memory_l1.id`（uuid）为稳定标识，与 Retriever 的 `L1DocView`/DocId::L1
///   一致；L2 事件以事件表 INTEGER 主键标识。
/// - `Pool` 表示 keyword_pool 中的词典词条自身（无业务文档关联），用于词典 / 模糊扩展。
///
/// # 为什么用 enum 而非多个结构体
///
/// 编译期内嵌 tag + data（无虚表指针），`match` 可由编译器穷尽检查，
/// serde 序列化友好，与既有 `keyword_refs.doc_type` 口径一一对应。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeywordRef {
    /// 指向 L1 摘要的引用
    L1 {
        /// L1 摘要 id（uuid，与 memory_l1.id 一致）
        id: uuid::Uuid,
        /// 所属 persona 的 uid
        persona_uid: String,
    },
    /// 指向 L2 事件的引用
    L2 {
        /// L2 事件 id（事件表 INTEGER 主键）
        id: i64,
        /// 所属 persona 的 uid
        persona_uid: String,
    },
    /// 关键词池中的词典词条定义（无业务文档引用）
    Pool {
        /// 标准化后的关键词文本
        keyword: String,
    },
}

impl KeywordRef {
    /// 返回文档类型标识字符串（供 DB 查询和日志使用）。
    pub fn doc_type(&self) -> &'static str {
        match self {
            Self::L1 { .. } => "l1",
            Self::L2 { .. } => "l2",
            Self::Pool { .. } => "pool",
        }
    }

    /// 返回业务文档 id 的文本形态（与 `keyword_refs.doc_id` 落库口径一致）。
    ///
    /// - `L1`/`L2`: 返回主键文本（uuid / i64）。
    /// - `Pool`: None（词典词条无业务文档 id）。
    pub fn doc_id_text(&self) -> Option<String> {
        match self {
            Self::L1 { id, .. } => Some(id.to_string()),
            Self::L2 { id, .. } => Some(id.to_string()),
            Self::Pool { .. } => None,
        }
    }

    /// 返回所属 persona_uid（L1/L2 有值，Pool 为 None）。
    pub fn persona_uid(&self) -> Option<&str> {
        match self {
            Self::L1 { persona_uid, .. } | Self::L2 { persona_uid, .. } => {
                Some(persona_uid.as_str())
            }
            Self::Pool { .. } => None,
        }
    }

    /// 返回单行可读标识（用于日志，不含文档正文，符合隐私红线）。
    pub fn label(&self) -> String {
        match self {
            Self::L1 { id, .. } => format!("l1:{id}"),
            Self::L2 { id, .. } => format!("l2:{id}"),
            Self::Pool { keyword } => format!("pool:{keyword}"),
        }
    }
}

// =========================================================
// MatchStrategy — 字面匹配策略
// =========================================================

/// 关键词匹配策略——KeywordIndex 检索时使用的字面匹配口径。
///
/// 设计决策（keyword-design §3.5）：**去除 Prefix 匹配**——前缀匹配是 UI 层补全功能
/// 而非索引层检索策略，子串匹配（Substring）已覆盖全部有意义的"部分匹配"需求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatchStrategy {
    /// 精确匹配：仅当查询 token 与索引 token 完全相等时命中
    Exact,
    /// 子串匹配：查询 token 是索引 token 的子串时命中（如查"工作"命中"工作压力"）
    Substring,
}

impl MatchStrategy {
    /// 返回策略的简短标识（用于日志与配置）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Substring => "substring",
        }
    }
}

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
mod tests {
    use super::*;
    use serde_json;

    // ── KeywordToken 测试 ──

    /// KeywordToken::new 规范化：中文保留 / 英文小写 / trim 空白
    #[test]
    fn keyword_token_normalization_cases() {
        let cases = [
            ("工作压力", "工作压力"),
            ("Work Stress", "work stress"),
            ("DeepSeek-API", "deepseek-api"),
            ("  职业倦怠  ", "职业倦怠"),
        ];
        for (input, expected) in cases {
            let token = KeywordToken::new(input).expect("关键词应能构造");
            assert_eq!(token.as_str(), expected, "input={input:?}");
        }
    }

    /// KeywordToken::new 无效输入：空/纯空白/超长 → None；边界长度 → Some
    #[test]
    fn keyword_token_invalid_inputs() {
        for bad in ["", "   ", "\t\n", &"x".repeat(257)] {
            assert!(KeywordToken::new(bad).is_none(), "input 应被拒绝");
        }
        let boundary = "x".repeat(256);
        let token = KeywordToken::new(&boundary);
        assert!(token.is_some());
        assert_eq!(token.unwrap().len(), 256);
    }

    /// Display 输出与 as_str 一致
    #[test]
    fn keyword_token_display() {
        let token = KeywordToken::new("人际关系").unwrap();
        assert_eq!(format!("{}", token), "人际关系");
    }

    /// PartialEq 比较
    #[test]
    fn keyword_token_partial_eq() {
        let a = KeywordToken::new("Work").unwrap();
        let b = KeywordToken::new("work").unwrap();
        assert_eq!(a, b);
    }

    /// Hash 一致性（相同标准化结果应 hash 相同）
    #[test]
    fn keyword_token_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(KeywordToken::new("Work").unwrap());
        set.insert(KeywordToken::new("work").unwrap());
        assert_eq!(set.len(), 1, "相同标准化结果应去重");
    }

    /// Serialize + Deserialize 往返
    #[test]
    fn keyword_token_serde_roundtrip() {
        let token = KeywordToken::new("职业倦怠").unwrap();
        let json = serde_json::to_string(&token).unwrap();
        let deserialized: KeywordToken = serde_json::from_str(&json).unwrap();
        assert_eq!(token, deserialized);
    }

    /// into_inner 消费 self（From<KeywordToken> for String 委托同一实现）
    #[test]
    fn keyword_token_into_inner() {
        let token = KeywordToken::new("测试").unwrap();
        let s: String = token.into_inner();
        assert_eq!(s, "测试");
    }

    /// AsRef<str>
    #[test]
    fn keyword_token_as_ref_str() {
        let token = KeywordToken::new("test").unwrap();
        let s: &str = token.as_ref();
        assert_eq!(s, "test");
    }

    // ── KeywordSet 测试 ──

    /// 空集合
    #[test]
    fn keyword_set_empty() {
        let set = KeywordSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    /// 插入去重
    #[test]
    fn keyword_set_dedup() {
        let mut set = KeywordSet::new();
        assert!(set.insert(KeywordToken::new("压力").unwrap()));
        assert!(
            !set.insert(KeywordToken::new("压力").unwrap()),
            "重复插入应返回 false"
        );
        assert_eq!(set.len(), 1);
    }

    /// 保留插入顺序
    #[test]
    fn keyword_set_order() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("工作").unwrap());
        set.insert(KeywordToken::new("压力").unwrap());
        set.insert(KeywordToken::new("倦怠").unwrap());
        let tokens: Vec<&str> = set.iter().map(|t| t.as_str()).collect();
        assert_eq!(tokens, vec!["工作", "压力", "倦怠"]);
    }

    /// from_iter
    #[test]
    fn keyword_set_from_iter() {
        let tokens = vec![
            KeywordToken::new("A").unwrap(),
            KeywordToken::new("B").unwrap(),
            KeywordToken::new("A").unwrap(), // 重复
        ];
        let set: KeywordSet = tokens.into_iter().collect();
        assert_eq!(set.len(), 2);
    }

    /// into_iter 消费
    #[test]
    fn keyword_set_into_iter() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("X").unwrap());
        set.insert(KeywordToken::new("Y").unwrap());
        let strings: Vec<String> = set.into_iter().map(|t| t.into_inner()).collect();
        assert_eq!(strings, vec!["x", "y"]);
    }

    /// contains
    #[test]
    fn keyword_set_contains() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("测试").unwrap());
        assert!(set.contains(&KeywordToken::new("测试").unwrap()));
        assert!(!set.contains(&KeywordToken::new("不存在").unwrap()));
    }

    /// extend
    #[test]
    fn keyword_set_extend() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("A").unwrap());
        let more = vec![
            KeywordToken::new("B").unwrap(),
            KeywordToken::new("C").unwrap(),
        ];
        set.extend(more);
        assert_eq!(set.len(), 3);
    }

    /// Serialize + Deserialize 往返
    #[test]
    fn keyword_set_serde_roundtrip() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("a").unwrap());
        set.insert(KeywordToken::new("b").unwrap());
        let json = serde_json::to_string(&set).unwrap();
        let deserialized: KeywordSet = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 2);
    }

    // ── KeywordStatus 测试 ──

    /// Canonical 默认值和标识
    #[test]
    fn keyword_status_canonical() {
        let status = KeywordStatus::Canonical;
        assert!(status.is_canonical());
        assert_eq!(status.as_str(), "canonical");
        assert!(status.canonical_id().is_none());
    }

    /// Alias 构造和查询
    #[test]
    fn keyword_status_alias() {
        let status = KeywordStatus::Alias { canonical_id: 42 };
        assert!(!status.is_canonical());
        assert_eq!(status.as_str(), "alias");
        assert_eq!(status.canonical_id(), Some(42));
    }

    /// Pending 构造和查询
    #[test]
    fn keyword_status_pending() {
        let status = KeywordStatus::Pending {
            suggested_canonical_id: 100,
        };
        assert!(!status.is_canonical());
        assert_eq!(status.as_str(), "pending");
        assert_eq!(status.canonical_id(), Some(100));
    }

    /// 默认值为 Canonical
    #[test]
    fn keyword_status_default() {
        let status: KeywordStatus = Default::default();
        assert_eq!(status, KeywordStatus::Canonical);
    }

    /// Serialize + Deserialize 往返（Canonical）
    #[test]
    fn keyword_status_serde_canonical() {
        let status = KeywordStatus::Canonical;
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: KeywordStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deserialized);
    }

    /// Serialize + Deserialize 往返（Alias）
    #[test]
    fn keyword_status_serde_alias() {
        let status = KeywordStatus::Alias { canonical_id: 7 };
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: KeywordStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deserialized);
        // 验证字段值
        match deserialized {
            KeywordStatus::Alias { canonical_id } => assert_eq!(canonical_id, 7),
            _ => panic!("应为 Alias"),
        }
    }

    // ── KeywordRef 测试 ──

    /// KeywordRef 各变体的 doc_type/doc_id_text/persona_uid/label 查询
    #[test]
    fn keyword_ref_variants() {
        let l1_id = uuid::Uuid::new_v4();
        let cases = vec![
            (
                KeywordRef::L1 {
                    id: l1_id,
                    persona_uid: "p1".to_string(),
                },
                "l1",
                Some(l1_id.to_string()),
                Some("p1"),
            ),
            (
                KeywordRef::L2 {
                    id: 456,
                    persona_uid: "p2".to_string(),
                },
                "l2",
                Some("456".to_string()),
                Some("p2"),
            ),
            (
                KeywordRef::Pool {
                    keyword: "测试词".to_string(),
                },
                "pool",
                None,
                None,
            ),
        ];
        for (r, dt, did, pu) in cases {
            assert_eq!(r.doc_type(), dt);
            assert_eq!(r.doc_id_text(), did);
            assert_eq!(r.persona_uid(), pu);
            // label 用于日志，前缀应与 doc_type 一致
            assert!(
                r.label().starts_with(dt),
                "label 应带 {dt} 前缀: {}",
                r.label()
            );
        }
    }

    /// Serialize + Deserialize 往返
    #[test]
    fn keyword_ref_serde_roundtrip() {
        let cases = vec![
            KeywordRef::L1 {
                id: uuid::Uuid::new_v4(),
                persona_uid: "u1".into(),
            },
            KeywordRef::L2 {
                id: 2,
                persona_uid: "u2".into(),
            },
            KeywordRef::Pool {
                keyword: "kw".into(),
            },
        ];
        for r in cases {
            let json = serde_json::to_string(&r).unwrap();
            let deserialized: KeywordRef = serde_json::from_str(&json).unwrap();
            assert_eq!(r, deserialized, "JSON 往返失败: {}", json);
        }
    }

    /// KeywordRef::label 不含文档正文（隐私：仅类型 + 主键）
    #[test]
    fn keyword_ref_label_has_no_summary_text() {
        let r = KeywordRef::L2 {
            id: 7,
            persona_uid: "user-0001".into(),
        };
        assert_eq!(r.label(), "l2:7");
    }

    // ── MatchStrategy 测试 ──

    /// MatchStrategy 仅 Exact/Substring（无 Prefix），as_str 稳定
    #[test]
    fn match_strategy_variants() {
        assert_eq!(MatchStrategy::Exact.as_str(), "exact");
        assert_eq!(MatchStrategy::Substring.as_str(), "substring");
        assert_ne!(MatchStrategy::Exact, MatchStrategy::Substring);
    }

    /// MatchStrategy serde 往返
    #[test]
    fn match_strategy_serde_roundtrip() {
        for s in [MatchStrategy::Exact, MatchStrategy::Substring] {
            let json = serde_json::to_string(&s).unwrap();
            let back: MatchStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    // ── KeywordQuery 测试 ──

    /// 默认构建参数：Exact / top_k=10 / persona 透传
    #[test]
    fn keyword_query_builder_defaults() {
        let q = KeywordQuery::builder(Some("user-0001".into())).build();
        assert_eq!(q.strategy, MatchStrategy::Exact);
        assert_eq!(q.top_k, 10);
        assert_eq!(q.persona_uid.as_deref(), Some("user-0001"));
        assert!(q.keywords.is_empty());
    }

    /// top_k 钳制：0 / 超大值都收敛到 1..=100
    #[test]
    fn keyword_query_top_k_clamped() {
        let low = KeywordQuery::builder(None).top_k(0).build();
        assert_eq!(low.top_k, 1);
        let high = KeywordQuery::builder(None).top_k(9999).build();
        assert_eq!(high.top_k, MAX_QUERY_TOP_K);
        let ok = KeywordQuery::builder(None).top_k(50).build();
        assert_eq!(ok.top_k, 50);
    }

    /// 关键词集合与策略可配置
    #[test]
    fn keyword_query_full_build() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("工作压力").unwrap());
        set.insert(KeywordToken::new("加班").unwrap());
        let q = KeywordQuery::builder(Some("user-1".into()))
            .with_keywords(set)
            .strategy(MatchStrategy::Substring)
            .top_k(5)
            .build();
        assert_eq!(q.keywords.len(), 2);
        assert_eq!(q.strategy, MatchStrategy::Substring);
        assert_eq!(q.top_k, 5);
    }

    /// builder.keywords_from / add_keyword 便捷路径
    #[test]
    fn keyword_query_token_collection_helpers() {
        let q = KeywordQuery::builder(None)
            .keywords_from(vec![
                KeywordToken::new("A").unwrap(),
                KeywordToken::new("B").unwrap(),
                KeywordToken::new("A").unwrap(), // 去重
            ])
            .add_keyword(KeywordToken::new("C").unwrap())
            .build();
        assert_eq!(q.keywords.len(), 3);
    }

    /// KeywordQuery serde 往返（构造钳制后不变量保持）
    #[test]
    fn keyword_query_serde_roundtrip() {
        let q = KeywordQuery::builder(Some("p".into()))
            .keywords_from(vec![KeywordToken::new("测试").unwrap()])
            .top_k(3)
            .build();
        let json = serde_json::to_string(&q).unwrap();
        let back: KeywordQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }
}
