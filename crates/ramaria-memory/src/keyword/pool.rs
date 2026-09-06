//! crates/ramaria-memory/src/keyword/pool.rs — 关键词词典池（三态状态机）
//!
//! 设计特点（keyword-design §6.1，P3 解决）:
//! - `KeywordPool`: 关键词词典的内存形态，支撑别名归一化三态流转
//! - 三态: `Canonical`（规范词）/ `Alias{canonical_id}`（已确认别名）/
//!   `Pending{suggested_canonical_id}`（待确认别名）
//! - `confirm_alias`: Pending → Alias（用户确认合并）
//! - `reject_alias`: Pending → Canonical（用户驳回，晋升为独立规范词）
//! - `upsert`: 词条累积（use_count+1 / last_used_at 刷新），不感知别名关系
//!
//! 职责边界:
//! - 本模块为**纯内存状态机**，零 I/O；与存储层的映射（keyword_pool 表 → 词条 /
//!   持久化迁移）由调用方（storage repo + 接线层）负责
//! - pending 冲突的"相似度"由 AliasManager 计算后随登记写入，本池只按行存 Optional
//!
//! 状态机图:
//! ```text
//! Unknown
//!   │ upsert（首次出现 → 插入为 Canonical）
//!   ▼
//! Canonical ────────────────┐
//!   ▲                       │ AliasManager 检测高度相似 → 登记为 Pending
//!   │ reject_alias          ▼
//!   └── (promote) ←── Pending{suggested_canonical_id}
//!                          │ confirm_alias
//!                          ▼
//!                      Alias{canonical_id}
//! ```

use ramaria_core::keyword::{KeywordStatus, KeywordToken};

// =========================================================
// 词条与冲突类型
// =========================================================

/// 关键词池词条（对应 keyword_pool 表一行）。
#[derive(Debug, Clone)]
pub struct PoolEntry {
    /// keyword_pool.rowid
    pub rowid: i64,
    /// 标准化关键词
    pub token: KeywordToken,
    /// 使用次数（每次出现 +1）
    pub use_count: i64,
    /// 最近使用时间（Unix 毫秒）
    pub last_used_at: i64,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 别名归一化三态
    pub status: KeywordStatus,
}

/// 待确认的别名冲突（Pending 词条展开为可读冲突记录）。
#[derive(Debug, Clone)]
pub struct AliasConflict {
    /// 别名词条行 id（keyword_pool.rowid）
    pub alias_id: i64,
    /// 别名文本
    pub alias_token: KeywordToken,
    /// 建议合并到的规范词行 id
    pub canonical_id: i64,
    /// 建议合并到的规范词文本
    pub canonical_token: KeywordToken,
    /// 相似度（由 AliasManager 计算；DB 无该列时经注册方附上，可为 None）
    pub similarity: Option<f64>,
    /// 登记时间（Unix 毫秒）
    pub created_at: i64,
}

// =========================================================
// KeywordPool
// =========================================================

/// 关键词词典池——管理词条累积与别名归一化的三态流转。
///
/// # 线程模型
///
/// 非 Send + Sync 的纯内存结构，由上层在单线程上下文持有，或以 `Mutex` 保护。
#[derive(Debug, Clone, Default)]
pub struct KeywordPool {
    /// 词条表（rowid 语义与 keyword_pool 表一致）
    entries: Vec<PoolEntry>,
}

impl KeywordPool {
    /// 创建空池。
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// 从词条列表构建池（批量装载，供启动加载 / 测试构造）。
    pub fn from_entries(entries: Vec<PoolEntry>) -> Self {
        Self { entries }
    }

    /// 池内词条总数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 池是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 词条只读迭代。
    pub fn iter(&self) -> impl Iterator<Item = &PoolEntry> {
        self.entries.iter()
    }

    /// 按行 id 取词条。
    pub fn entry_by_id(&self, rowid: i64) -> Option<&PoolEntry> {
        self.entries.iter().find(|e| e.rowid == rowid)
    }

    /// 按文本取词条。
    pub fn entry_by_token(&self, token: &KeywordToken) -> Option<&PoolEntry> {
        self.entries.iter().find(|e| &e.token == token)
    }

    // =========================================================
    // 累积（不感知别名关系）
    // =========================================================

    /// 插入或更新一个关键词（use_count +1，刷新 last_used_at）。
    ///
    /// 已存在（无论 Canonical/Alias/Pending）→ 累加；不存在 → 插入为 Canonical。
    /// 插入的行 id 使用自增序列（超出既有最大行 id +1），模拟 SQLite rowid 语义。
    pub fn upsert(&mut self, token: KeywordToken, now_ms: i64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.token == token) {
            entry.use_count += 1;
            entry.last_used_at = now_ms;
            return;
        }
        let next_id = self
            .entries
            .iter()
            .map(|e| e.rowid)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1);
        self.entries.push(PoolEntry {
            rowid: next_id,
            token,
            use_count: 1,
            last_used_at: now_ms,
            created_at: now_ms,
            status: KeywordStatus::Canonical,
        });
    }

    // =========================================================
    // 别名解析与查询
    // =========================================================

    /// 解析词条指向的规范词：Canonical → 自身；Alias/Pending → canonical_id 指向词条。
    pub fn resolve(&self, token: &KeywordToken) -> Option<&KeywordToken> {
        let entry = self.entry_by_token(token)?;
        match entry.status {
            KeywordStatus::Canonical => Some(&entry.token),
            KeywordStatus::Alias { canonical_id }
            | KeywordStatus::Pending {
                suggested_canonical_id: canonical_id,
            } => self.entry_by_id(canonical_id).map(|c| &c.token),
        }
    }

    /// 规范词列表（状态 Canonical 且含 canonical 语义词条），按 use_count 降序。
    pub fn list_canonicals(&self) -> Vec<&KeywordToken> {
        let mut list: Vec<&PoolEntry> = self.entries.iter().collect();
        list.sort_by(|a, b| {
            b.use_count
                .cmp(&a.use_count)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        list.into_iter()
            .filter(|e| e.status.is_canonical())
            .map(|e| &e.token)
            .collect()
    }

    /// 待确认别名冲突列表（Pending 词条），按 created_at 升序（先进先审）。
    pub fn pending_conflicts(&self) -> Vec<AliasConflict> {
        let mut pending: Vec<&PoolEntry> = self
            .entries
            .iter()
            .filter(|e| matches!(e.status, KeywordStatus::Pending { .. }))
            .collect();
        pending.sort_by_key(|e| e.created_at);

        pending
            .into_iter()
            .filter_map(|alias| match alias.status {
                KeywordStatus::Pending {
                    suggested_canonical_id,
                } => self.entry_by_id(suggested_canonical_id).map(|canonical| {
                    AliasConflict {
                        alias_id: alias.rowid,
                        alias_token: alias.token.clone(),
                        canonical_id: canonical.rowid,
                        canonical_token: canonical.token.clone(),
                        similarity: None, // 相似度由 AliasManager 登记时补充（见类型 doc）
                        created_at: alias.created_at,
                    }
                }),
                _ => None,
            })
            .collect()
    }

    // =========================================================
    // 状态机迁移
    // =========================================================

    /// 确认别名：Pending → Alias（alias_id 指向原 suggested canonical）。
    ///
    /// 返回是否发生迁移（词条不存在 / 非 Pending → false）。
    pub fn confirm_alias(&mut self, alias_id: i64) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.rowid == alias_id) else {
            return false;
        };
        match entry.status {
            KeywordStatus::Pending {
                suggested_canonical_id,
            } => {
                entry.status = KeywordStatus::Alias {
                    canonical_id: suggested_canonical_id,
                };
                true
            }
            _ => false,
        }
    }

    /// 驳回别名：Pending → Canonical（晋升为独立规范词，解除与 canonical 的关系）。
    ///
    /// 返回是否发生迁移（词条不存在 / 非 Pending → false）。
    pub fn reject_alias(&mut self, alias_id: i64) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.rowid == alias_id) else {
            return false;
        };
        match entry.status {
            KeywordStatus::Pending { .. } => {
                entry.status = KeywordStatus::Canonical;
                true
            }
            _ => false,
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn token(s: &str) -> KeywordToken {
        KeywordToken::new(s).unwrap()
    }

    fn entry(rowid: i64, text: &str, use_count: i64, status: KeywordStatus) -> PoolEntry {
        PoolEntry {
            rowid,
            token: token(text),
            use_count,
            last_used_at: 0,
            created_at: rowid, // rowid 即时间序，便于断言稳定
            status,
        }
    }

    fn sample_pool() -> KeywordPool {
        KeywordPool::from_entries(vec![
            entry(1, "工作压力", 10, KeywordStatus::Canonical),
            entry(
                2,
                "职场焦虑",
                8,
                KeywordStatus::Pending {
                    suggested_canonical_id: 1,
                },
            ),
            entry(3, "职业倦怠", 5, KeywordStatus::Alias { canonical_id: 1 }),
        ])
    }

    // ---- 构造与 upsert ----

    #[test]
    fn empty_pool() {
        let pool = KeywordPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert!(pool.list_canonicals().is_empty());
        assert!(pool.pending_conflicts().is_empty());
    }

    #[test]
    fn upsert_new_inserts_canonical() {
        let mut pool = KeywordPool::new();
        pool.upsert(token("爬山"), 1000);
        assert_eq!(pool.len(), 1);
        let e = pool.entry_by_token(&token("爬山")).unwrap();
        assert_eq!(e.use_count, 1);
        assert!(e.status.is_canonical());
    }

    #[test]
    fn upsert_existing_increments_count() {
        let mut pool = sample_pool();
        pool.upsert(token("工作压力"), 5000);
        let e = pool.entry_by_token(&token("工作压力")).unwrap();
        assert_eq!(e.use_count, 11);
        assert_eq!(e.last_used_at, 5000);
    }

    // ---- 解析 ----

    #[test]
    fn resolve_canonical_and_alias() {
        let pool = sample_pool();
        assert_eq!(
            pool.resolve(&token("工作压力")).unwrap().as_str(),
            "工作压力"
        );
        // Pending 别名在确认前也可解析到建议规范词
        assert_eq!(
            pool.resolve(&token("职场焦虑")).unwrap().as_str(),
            "工作压力"
        );
        // 已确认别名解析到规范词
        assert_eq!(
            pool.resolve(&token("职业倦怠")).unwrap().as_str(),
            "工作压力"
        );
        assert!(pool.resolve(&token("不存在")).is_none());
    }

    #[test]
    fn list_canonicals_sorted_by_use_count() {
        let pool = sample_pool();
        let canonicals = pool.list_canonicals();
        assert_eq!(canonicals.len(), 1); // 只有 工作压力 是 Canonical
        assert_eq!(canonicals[0].as_str(), "工作压力");
    }

    // ---- pending 冲突 ----

    #[test]
    fn pending_conflicts_lists_pending_only() {
        let pool = sample_pool();
        let conflicts = pool.pending_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].alias_id, 2);
        assert_eq!(conflicts[0].alias_token.as_str(), "职场焦虑");
        assert_eq!(conflicts[0].canonical_id, 1);
        assert_eq!(conflicts[0].canonical_token.as_str(), "工作压力");
        assert!(conflicts[0].similarity.is_none());
    }

    // ---- 状态机迁移 ----

    #[test]
    fn confirm_alias_transitions_pending_to_alias() {
        let mut pool = sample_pool();
        assert!(pool.confirm_alias(2));
        let e = pool.entry_by_id(2).unwrap();
        assert_eq!(e.status, KeywordStatus::Alias { canonical_id: 1 });
        // pending 冲突随之消失
        assert!(pool.pending_conflicts().is_empty());
        // 非 Pending / 不存在的 id 不迁移
        assert!(!pool.confirm_alias(1));
        assert!(!pool.confirm_alias(99));
    }

    #[test]
    fn reject_alias_promotes_pending_to_canonical() {
        let mut pool = sample_pool();
        assert!(pool.reject_alias(2));
        let e = pool.entry_by_id(2).unwrap();
        assert!(e.status.is_canonical());
        // 晋升后仍是规范词且保留使用量
        assert_eq!(pool.list_canonicals().len(), 2);
        assert!(!pool.reject_alias(1));
        assert!(!pool.reject_alias(99));
    }

    #[test]
    fn reject_preserves_use_count() {
        let mut pool = sample_pool();
        assert!(pool.reject_alias(2));
        let e = pool.entry_by_id(2).unwrap();
        assert_eq!(e.use_count, 8);
    }
}
