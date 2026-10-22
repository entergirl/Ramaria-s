//! crates/ramaria-core/src/keyword/rows.rs - 别名状态、词条行与引用类型
//!
//! 设计特点:
//! - KeywordStatus 以三态描述规范词、已确认别名与待审核别名
//! - KeywordPoolRow / PendingAliasRow 为 keyword_pool 的纯数据行形态
//! - KeywordRef 内嵌文档类型与 persona 归属，作为检索结果标识
//! - MatchStrategy 只保留 Exact / Substring 两种字面匹配口径

use serde::{Deserialize, Serialize};

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
// PendingAliasRow — 待确认别名冲突行
// =========================================================

/// 待确认别名冲突行（`alias_status='pending'`，join 出规范词文本）。
///
/// 职责:
/// - 供存储层返回 keyword_pool 中待裁决的别名词条，上层（关键词用例）据此
///   展示冲突并执行确认 / 驳回迁移；零 I/O 纯数据行。
///
/// 字段约定:
/// - `alias_id`: 别名词条 rowid（`keyword_pool` 的 INTEGER 主键），确认 / 驳回按此定位词条。
/// - `alias_keyword`: 别名文本（标准化后）。
/// - `canonical_id`: 建议合并到的规范词 rowid。
/// - `canonical_keyword`: 规范词文本（join 解析，展示用途）。
/// - `created_at`: 登记时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAliasRow {
    /// 别名词条 rowid
    pub alias_id: i64,
    /// 别名文本
    pub alias_keyword: String,
    /// 建议合并到的规范词 rowid
    pub canonical_id: i64,
    /// 规范词文本
    pub canonical_keyword: String,
    /// 登记时间（Unix 毫秒）
    pub created_at: i64,
}

// =========================================================
// KeywordRef — 倒排索引引用枚举
// =========================================================

/// 关键词倒排引用——标识一个关键词出现在哪些业务文档 / 词典词条中。
///
/// # 语义约定（M3 消费路径定稿）
///
/// - 作为 `ramaria-memory` KeywordIndex / CompositeIndex 检索结果的**文档标识**返回，
///   调用方依据 `doc_type()` + `doc_id_text()` 回查业务文档内容。
/// - L1 摘要以 `memory_l1.id`（uuid）为稳定标识，与 Retriever 的 `L1DocView`/DocId::L1
///   一致；L2 事件以事件表 INTEGER 主键标识。
/// - `Pool` 表示 keyword_pool 中的词典词条自身（无业务文档关联），用于词典 / 模糊扩展。
///
/// # 为什么用 enum 而非多个结构体
///
/// 编译期内嵌 tag + data（无虚表指针），`match` 可由编译器穷尽检查，
/// serde 序列化友好，`doc_type()` 取值与存储侧文档类型口径一致。
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

    /// 返回业务文档 id 的文本形态。
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
