//! crates/ramaria-core/src/keyword/token.rs - 关键词 Newtype 与去重集合
//!
//! 设计特点:
//! - KeywordToken 构造时完成 trim + ASCII 小写 + 非空与长度校验
//! - 保留 from_validated 旁路供已标准化 token 复用，不做二次构造
//! - KeywordSet 以 Vec 线性去重，保留插入顺序
//! - 纯类型实现，零 I/O

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
