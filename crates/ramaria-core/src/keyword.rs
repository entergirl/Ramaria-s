//! rust/crates/ramaria-core/src/keyword.rs - Ramaria 关键词类型系统
//!
//! 设计特点:
//! - `KeywordToken`: 标准化关键词 Newtype，自动 trim + 小写 + 非空校验
//! - `KeywordSet`: 保留插入顺序的去重集合，驱动 TopicBatcher 关键词图构建
//! - `KeywordStatus`: 三态枚举（Canonical / Alias / Pending），支撑别名归一化管线
//! - `KeywordRef`: 倒排索引引用枚举（L1/L2/Pool），关联关键词与业务文档
//! - 纯类型层，零 I/O，零外部依赖（仅 serde），完全符合 ramaria-core 零 I/O 约束

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
/// - 只能通过 `new()` 构造（保证标准化），不可直接访问内部 String
/// - `as_str()` 只读访问，不暴露修改能力
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

    /// 返回排序后的关键词列表副本（按字母顺序，用于一致性输出）。
    pub fn sorted(&self) -> Vec<KeywordToken> {
        let mut sorted = self.tokens.clone();
        sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        sorted
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

    /// 返回底层向量引用（供序列化用）。
    pub fn as_vec(&self) -> &[KeywordToken] {
        &self.tokens
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
// KeywordRef — 倒排索引引用枚举
// =========================================================

/// 关键词倒排引用——标识一个关键词出现在哪些业务文档中。
///
/// 职责:
/// - 支撑 `keyword_refs` 倒排索引表的类型安全映射
/// - 供精确匹配检索（`search_exact`）和关键词溯源使用
///
/// 变体说明:
/// - `L1`: 关键词出现在某条 L1 摘要中
/// - `L2`: 关键词出现在某个 L2 事件的 keywords 字段中
/// - `Pool`: 关键词自身在 keyword_pool 中的定义（无业务文档关联）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeywordRef {
    /// 指向 L1 摘要的引用
    L1 {
        /// L1 摘要的 id（UUID 字符串格式）
        id: i64,
        /// 所属 persona 的 uid
        persona_uid: String,
    },
    /// 指向 L2 事件的引用
    L2 {
        /// L2 事件的 id（INTEGER 主键）
        id: i64,
        /// 所属 persona 的 uid
        persona_uid: String,
    },
    /// 关键词池中的词条定义（无业务文档引用）
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

    /// 返回文档 ID（供 DB 写入使用）。
    ///
    /// 返回:
    /// - `L1`/`L2`: Some(id)
    /// - `Pool`: None（关键词池条目无文档 ID）
    pub fn doc_id(&self) -> Option<i64> {
        match self {
            Self::L1 { id, .. } => Some(*id),
            Self::L2 { id, .. } => Some(*id),
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
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    // ── KeywordToken 测试 ──

    /// 正常中文关键词
    #[test]
    fn keyword_token_normal_chinese() {
        let token = KeywordToken::new("工作压力").expect("中文关键词应能构造");
        assert_eq!(token.as_str(), "工作压力");
    }

    /// 英文自动小写
    #[test]
    fn keyword_token_english_lowercase() {
        let token = KeywordToken::new("Work Stress").expect("英文关键词应能构造");
        assert_eq!(token.as_str(), "work stress");
    }

    /// 混合大小写英文
    #[test]
    fn keyword_token_mixed_case() {
        let token = KeywordToken::new("DeepSeek-API").expect("混合大小写应能构造");
        assert_eq!(token.as_str(), "deepseek-api");
    }

    /// 前后空白被 trim
    #[test]
    fn keyword_token_trim_whitespace() {
        let token = KeywordToken::new("  职业倦怠  ").expect("含空白关键词应能构造");
        assert_eq!(token.as_str(), "职业倦怠");
    }

    /// 空字符串返回 None
    #[test]
    fn keyword_token_empty_returns_none() {
        assert!(KeywordToken::new("").is_none());
    }

    /// 纯空白返回 None
    #[test]
    fn keyword_token_whitespace_only_returns_none() {
        assert!(KeywordToken::new("   ").is_none());
        assert!(KeywordToken::new("\t\n").is_none());
    }

    /// 超长字符串返回 None
    #[test]
    fn keyword_token_too_long_returns_none() {
        let long_str = "x".repeat(257);
        assert!(KeywordToken::new(&long_str).is_none());
    }

    /// 边界长度（256 字符）应能构造
    #[test]
    fn keyword_token_boundary_length() {
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

    /// into_inner 消费 self
    #[test]
    fn keyword_token_into_inner() {
        let token = KeywordToken::new("测试").unwrap();
        let s: String = token.into_inner();
        assert_eq!(s, "测试");
    }

    /// From<KeywordToken> for String
    #[test]
    fn keyword_token_into_string() {
        let token = KeywordToken::new("测试").unwrap();
        let s: String = token.into();
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

    /// sorted 排序
    #[test]
    fn keyword_set_sorted() {
        let mut set = KeywordSet::new();
        set.insert(KeywordToken::new("工作").unwrap());
        set.insert(KeywordToken::new("压力").unwrap());
        let sorted = set.sorted();
        // 按 Unicode 码点排序：压(U+538B) < 工(U+5DE5)
        assert_eq!(sorted[0].as_str(), "压力");
        assert_eq!(sorted[1].as_str(), "工作");
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

    /// L1 引用构造和查询
    #[test]
    fn keyword_ref_l1() {
        let r = KeywordRef::L1 {
            id: 123,
            persona_uid: "p1".to_string(),
        };
        assert_eq!(r.doc_type(), "l1");
        assert_eq!(r.doc_id(), Some(123));
        assert_eq!(r.persona_uid(), Some("p1"));
    }

    /// L2 引用构造和查询
    #[test]
    fn keyword_ref_l2() {
        let r = KeywordRef::L2 {
            id: 456,
            persona_uid: "p2".to_string(),
        };
        assert_eq!(r.doc_type(), "l2");
        assert_eq!(r.doc_id(), Some(456));
        assert_eq!(r.persona_uid(), Some("p2"));
    }

    /// Pool 引用构造和查询
    #[test]
    fn keyword_ref_pool() {
        let r = KeywordRef::Pool {
            keyword: "测试词".to_string(),
        };
        assert_eq!(r.doc_type(), "pool");
        assert_eq!(r.doc_id(), None);
        assert_eq!(r.persona_uid(), None);
    }

    /// Serialize + Deserialize 往返
    #[test]
    fn keyword_ref_serde_roundtrip() {
        let cases = vec![
            KeywordRef::L1 {
                id: 1,
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
}
