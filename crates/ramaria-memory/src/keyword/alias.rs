//! crates/ramaria-memory/src/keyword/alias.rs - 关键词别名管理模块
//!
//! 设计特点:
//! - `AliasManager`: 内存级别名管理器，注册/查询/建议合并一站式
//! - 双缓存设计：`alias_to_canonical` 别名→规范词正向查询 + `canonical_id_by_text` 文本→ID 反向查询
//! - 合并建议引擎：按 `use_count` 分析同类关键词，输出"高使用量同义词应合并"的建议
//! - 纯逻辑层，不直接操作数据库（存储操作通过回调/注入实现）
//!
//! 预留给 keyword_refs 消费路径（v1.6 精确匹配检索）
//!
//! 用法:
//! ```
//! use ramaria_memory::keyword::alias::AliasManager;
//! let mut mgr = AliasManager::new();
//! mgr.register_alias("职场焦虑", 1, "工作压力");
//! assert_eq!(mgr.resolve_alias_text("职场焦虑"), Some("工作压力"));
//! ```

use std::collections::HashMap;

use ramaria_core::keyword::KeywordToken;

// =========================================================
// AliasManager
// =========================================================

/// 关键词别名管理器（内存缓存）。
///
/// 职责:
/// - 维护别名→规范词的映射缓存（内存 HashMap，启动时从 keyword_pool 加载）
/// - 提供别名注册、查询、注销操作
/// - 分析关键词使用量分布，输出合并建议
///
/// 状态:
/// - `alias_to_canonical`: 别名文本 → (规范词 KeywordToken, 规范词 keyword_pool.id)
/// - `canonical_id_by_text`: 规范词文本 → keyword_pool.id（反向查询，验证存在性）
/// - `use_counts`: 关键词文本 → 使用计数（从 DB 加载，用于合并建议引擎）
///
/// 线程安全:
/// - AliasManager 不是 Send + Sync，由上层通过 Mutex 保护
/// - 单一线程访问模式（后台任务/导入流程中串行使用）
///
/// 与 DB 的关系:
/// - 启动时从 `keyword_pool` 表加载所有 Canonical/Alias 条目到缓存
/// - 写入时同步写缓存 + 委托 DB 写入（通过外部回调）
/// - 查询时优先查缓存（避免每次查询穿透到 DB）
pub struct AliasManager {
    /// 别名文本 → (规范词 KeywordToken, 规范词 ID)
    alias_to_canonical: HashMap<String, (KeywordToken, i64)>,
    /// 规范词文本 → keyword_pool.id（反向查询）
    canonical_id_by_text: HashMap<String, i64>,
    /// 关键词使用计数（文本 → 使用次数）
    use_counts: HashMap<String, u32>,
}

impl AliasManager {
    /// 创建空的别名管理器。
    ///
    /// 说明:
    /// - 初始为空缓存，需调用 `load_from_entries` 或逐条 `register_alias` 填充。
    /// - 启动时建议从 keyword_pool 表批量加载。
    pub fn new() -> Self {
        Self {
            alias_to_canonical: HashMap::new(),
            canonical_id_by_text: HashMap::new(),
            use_counts: HashMap::new(),
        }
    }

    /// 注册一个别名映射。
    ///
    /// 参数:
    /// - `alias_text`: 别名文本（如 "职场焦虑"）。
    /// - `canonical_id`: 规范词在 keyword_pool 中的 id（INTEGER 主键）。
    /// - `canonical_text`: 规范词文本（如 "工作压力"）。
    ///
    /// 返回:
    /// - `Ok(())`: 注册成功。
    /// - `Err(String)`: 别名与规范词相同（无意义循环别名）。
    ///
    /// 说明:
    /// - 如果 `alias_text` 已注册，覆盖旧映射。
    /// - `canonical_text` 会自动通过 `KeywordToken::new()` 标准化。
    /// - 不自动写入 DB——由调用方在外部完成持久化。
    pub fn register_alias(
        &mut self,
        alias_text: &str,
        canonical_id: i64,
        canonical_text: &str,
    ) -> Result<(), String> {
        // 阻止循环别名：别名不能指向自身
        if alias_text == canonical_text {
            return Err(format!("别名不能指向自身: '{}' 与规范词相同", alias_text));
        }

        let canonical_token = KeywordToken::new(canonical_text)
            .ok_or_else(|| format!("规范词文本无效（空或纯空白）: '{}'", canonical_text))?;

        // 更新正向缓存：别名 → (规范词, ID)
        self.alias_to_canonical.insert(
            alias_text.to_string(),
            (canonical_token.clone(), canonical_id),
        );

        // 更新反向缓存：规范词文本 → ID（仅在首次或 ID 变更时）
        self.canonical_id_by_text
            .entry(canonical_token.as_str().to_string())
            .or_insert(canonical_id);

        tracing::debug!(
            alias = alias_text,
            canonical = %canonical_token,
            canonical_id,
            "别名注册成功"
        );

        Ok(())
    }

    /// 根据别名文本查询规范词。
    ///
    /// 参数:
    /// - `alias_text`: 待查询的别名文本。
    ///
    /// 返回:
    /// - `Some((KeywordToken, canonical_id))`: 查找到的规范词和 ID。
    /// - `None`: 未找到别名映射（该关键词本身就是规范词，或不存在）。
    ///
    /// 说明:
    /// - 只查缓存，不穿透 DB。
    /// - 调用方应先通过此方法查询，未命中时视关键词为 Canonical 状态。
    pub fn resolve_alias(&self, alias_text: &str) -> Option<(KeywordToken, i64)> {
        self.alias_to_canonical
            .get(alias_text)
            .map(|(token, id)| (token.clone(), *id))
    }

    /// 根据别名文本查询规范词文本（便捷方法）。
    ///
    /// 参数:
    /// - `alias_text`: 待查询的别名文本。
    ///
    /// 返回:
    /// - `Some(&str)`: 规范词文本。
    /// - `None`: 未找到别名映射。
    pub fn resolve_alias_text(&self, alias_text: &str) -> Option<&str> {
        self.alias_to_canonical
            .get(alias_text)
            .map(|(token, _)| token.as_str())
    }

    /// 注销别名映射。
    ///
    /// 参数:
    /// - `alias_text`: 要注销的别名文本。
    ///
    /// 返回:
    /// - `true`: 成功注销。
    /// - `false`: 别名不存在。
    ///
    /// 说明:
    /// - 不从 `canonical_id_by_text` 中删除规范词条目（其他别名可能仍使用）。
    pub fn unregister_alias(&mut self, alias_text: &str) -> bool {
        self.alias_to_canonical.remove(alias_text).is_some()
    }

    /// 根据规范词文本查询 ID。
    ///
    /// 返回:
    /// - `Some(i64)`: 规范词 ID。
    /// - `None`: 未缓存该规范词。
    pub fn canonical_id(&self, canonical_text: &str) -> Option<i64> {
        // 先尝试精确匹配，再尝试标准化后匹配
        self.canonical_id_by_text
            .get(canonical_text)
            .copied()
            .or_else(|| {
                KeywordToken::new(canonical_text)
                    .and_then(|t| self.canonical_id_by_text.get(t.as_str()).copied())
            })
    }

    /// 判断关键词是否为别名（而非规范词）。
    pub fn is_alias(&self, keyword_text: &str) -> bool {
        self.alias_to_canonical.contains_key(keyword_text)
    }

    /// 返回所有已注册的别名映射条目数。
    pub fn alias_count(&self) -> usize {
        self.alias_to_canonical.len()
    }

    /// 返回已缓存的规范词数。
    pub fn canonical_count(&self) -> usize {
        self.canonical_id_by_text.len()
    }

    // =========================================================
    // 使用量与合并建议
    // =========================================================

    /// 批量设置关键词使用计数（从 keyword_pool 加载）。
    ///
    /// 参数:
    /// - `counts`: 关键词文本 → 使用次数的映射。
    ///
    /// 说明:
    /// - 覆盖式设置（非增量累加）。
    /// - 建议在启动时从 keyword_pool 表 `SELECT keyword, use_count` 加载。
    pub fn load_use_counts(&mut self, counts: HashMap<String, u32>) {
        self.use_counts = counts;
        tracing::debug!(count = self.use_counts.len(), "关键词使用计数已加载");
    }

    /// 获取指定关键词的使用计数。
    ///
    /// 返回:
    /// - 使用次数（未记录时返回 0）。
    pub fn use_count(&self, keyword_text: &str) -> u32 {
        self.use_counts.get(keyword_text).copied().unwrap_or(0)
    }
}

// =========================================================
// 合并建议
// =========================================================

/// 合并建议——描述一对应合并的同义词。
#[derive(Debug, Clone, PartialEq)]
pub struct MergeSuggestion {
    /// 建议保留的规范词文本
    pub canonical_text: String,
    /// 规范词在 keyword_pool 中的 id
    pub canonical_id: i64,
    /// 建议合并的别名文本
    pub alias_text: String,
    /// 别名当前使用量
    pub alias_use_count: u32,
    /// 建议理由
    pub reason: String,
}

impl AliasManager {
    /// 分析关键词使用量分布，生成合并建议。
    ///
    /// 算法:
    /// 1. 按 `use_count DESC` 排序所有关键词。
    /// 2. 对每个高使用量关键词（≥ 阈值 5），检查是否有同义词已在别名系统中。
    /// 3. 对未注册的同义词对，输出合并建议。
    /// 4. 低使用量（< 3）的关键词不参与建议（噪声过滤）。
    ///
    /// 参数:
    /// - `min_use_for_suggestion`: 最小使用量阈值（默认 3），低于此值的关键词不参与。
    ///   建议使用 3 作为默认值，过滤仅出现 1-2 次的偶然用词。
    ///
    /// 返回:
    /// - 合并建议列表，按建议优先级降序排列（高使用量别名优先）。
    ///
    /// 说明:
    /// - 当前为简化实现：仅通过文本相似性（编辑距离 < 3 或共享前缀）找出可能的同义词。
    /// - 未来可接入 embedding 语义相似度提升匹配精度。
    pub fn suggest_merges(&self, min_use_for_suggestion: u32) -> Vec<MergeSuggestion> {
        if self.use_counts.is_empty() {
            return Vec::new();
        }

        // 按使用量降序排列关键词
        let mut sorted: Vec<(&String, &u32)> = self.use_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));

        let mut suggestions = Vec::new();

        // 对每个高使用量词，检查是否有同义词在别名系统中
        for (i, (keyword1, count1)) in sorted.iter().enumerate() {
            if **count1 < min_use_for_suggestion {
                continue; // 低使用量噪声过滤
            }

            // 已经作为规范词存在的不需要再建议
            if self.canonical_id_by_text.contains_key(keyword1.as_str()) {
                continue;
            }

            // 已经是别名的，检查其所指规范词是否是高使用量
            if let Some((canonical_token, canonical_id)) = self.resolve_alias(keyword1) {
                let canonical_text = canonical_token.as_str().to_string();
                let canonical_use = self.use_count(&canonical_text);
                if canonical_use < **count1 {
                    // 别名使用量高于规范词 → 建议将规范词更换为此别名（反向合并）
                    suggestions.push(MergeSuggestion {
                        canonical_text: keyword1.to_string(),
                        canonical_id,
                        alias_text: canonical_text.clone(),
                        alias_use_count: **count1,
                        reason: format!(
                            "别名 '{}' (使用 {} 次) 使用量高于当前规范词 '{}' ({} 次)，建议交换规范/别名角色",
                            keyword1, count1, canonical_text, canonical_use,
                        ),
                    });
                }
                continue;
            }

            // 检查是否与其他高使用量词相似（编辑距离或共有前缀）
            for (j, (keyword2, count2)) in sorted.iter().enumerate() {
                if j <= i {
                    continue;
                }
                if **count2 < min_use_for_suggestion {
                    continue;
                }

                // 跳过已注册为别名的
                if self.alias_to_canonical.contains_key(keyword2.as_str()) {
                    continue;
                }

                // 简单相似度判断：共享至少 2 个共同字或编辑距离 ≤ 2
                if is_similar_keyword(keyword1, keyword2) {
                    // 使用量高的作为规范词
                    let (canonical, alias) = if **count1 >= **count2 {
                        ((*keyword1).clone(), (*keyword2).clone())
                    } else {
                        ((*keyword2).clone(), (*keyword1).clone())
                    };

                    suggestions.push(MergeSuggestion {
                        canonical_text: canonical.clone(),
                        canonical_id: 0, // 尚未分配，调用方在写入 DB 时获取
                        alias_text: alias.clone(),
                        alias_use_count: *self.use_counts.get(alias.as_str()).unwrap_or(&0),
                        reason: format!(
                            "'{}' (使用 {} 次) 与 '{}' (使用 {} 次) 相似，建议合并到 '{}'",
                            keyword1, count1, keyword2, count2, canonical,
                        ),
                    });
                }
            }
        }

        // 按别名使用量降序排列建议
        suggestions.sort_by_key(|b| std::cmp::Reverse(b.alias_use_count));
        suggestions
    }

    /// 清空所有缓存（用于热重载）。
    pub fn clear(&mut self) {
        self.alias_to_canonical.clear();
        self.canonical_id_by_text.clear();
        self.use_counts.clear();
    }
}

impl Default for AliasManager {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// 相似度判断辅助函数
// =========================================================

/// 判断两个关键词是否相似（可能为同义词）。
///
/// 条件（满足任一即判定为相似）:
/// 1. 共享至少 2 个共同汉字（忽略顺序）。
/// 2. 编辑距离（Levenshtein）≤ 2。
/// 3. 一个完全包含另一个（如"职场压力"包含"压力"）。
///
/// 说明:
/// - 仅用于合并建议的初步筛选，不涉及 DB 写入。
/// - 可升级为 embedding cosine similarity。
fn is_similar_keyword(a: &str, b: &str) -> bool {
    if a == b {
        return false; // 完全相同不视为"相似"
    }

    // 条件 1：共享至少 2 个共同汉字
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    let common: usize = a_chars.iter().filter(|c| b_chars.contains(c)).count();
    if common >= 2 {
        return true;
    }

    // 条件 2：编辑距离 ≤ 2
    if levenshtein_distance(a, b) <= 2 {
        return true;
    }

    // 条件 3：一个包含另一个（如"职场工作压力"包含"工作压力"）
    if a.contains(b) || b.contains(a) {
        return true;
    }

    false
}

/// 计算两个字符串之间的 Levenshtein 编辑距离。
///
/// 说明:
/// - 使用动态规划 O(n·m) 实现。
/// - 仅用于合并建议引擎，不在热路径上（关键词集通常 < 1000）。
/// - 当任一字符串长度 > 20 时快速返回上限值（性能保护）。
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    // 性能保护：超长字符串快速返回
    if n.max(m) > 20 {
        return n.max(m);
    }

    // 标准 DP 实现
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate().take(n + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(m + 1) {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1) // 删除
                .min(dp[i][j - 1] + 1) // 插入
                .min(dp[i - 1][j - 1] + cost); // 替换
        }
    }

    dp[n][m]
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 别名注册与查询 ──

    /// 基本别名注册和查询
    #[test]
    fn register_and_resolve_alias() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("职场焦虑", 1, "工作压力").unwrap();
        let result = mgr.resolve_alias("职场焦虑");
        assert!(result.is_some());
        let (token, id) = result.unwrap();
        assert_eq!(token.as_str(), "工作压力");
        assert_eq!(id, 1);
    }

    /// 查询不存在的别名返回 None
    #[test]
    fn resolve_nonexistent_alias() {
        let mgr = AliasManager::new();
        assert!(mgr.resolve_alias("不存在的").is_none());
    }

    /// 循环别名被拒绝
    #[test]
    fn cyclic_alias_rejected() {
        let mut mgr = AliasManager::new();
        let result = mgr.register_alias("工作压力", 1, "工作压力");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("指向自身"));
    }

    /// 无效规范词文本被拒绝
    #[test]
    fn invalid_canonical_rejected() {
        let mut mgr = AliasManager::new();
        let result = mgr.register_alias("别名", 1, "");
        assert!(result.is_err());
    }

    /// 覆盖已有别名映射
    #[test]
    fn overwrite_alias() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("焦虑", 1, "工作压力").unwrap();
        mgr.register_alias("焦虑", 2, "职业倦怠").unwrap();
        let result = mgr.resolve_alias_text("焦虑");
        assert_eq!(result, Some("职业倦怠"));
    }

    /// 注销别名
    #[test]
    fn unregister_alias() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("a", 1, "b").unwrap();
        assert!(mgr.is_alias("a"));
        assert!(mgr.unregister_alias("a"));
        assert!(!mgr.is_alias("a"));
    }

    // ── 规范词 ID 查询 ──

    /// 通过文本查询规范词 ID
    #[test]
    fn canonical_id_lookup() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("a1", 42, "规范词").unwrap();
        assert_eq!(mgr.canonical_id("规范词"), Some(42));
    }

    /// 不存在的规范词返回 None
    #[test]
    fn nonexistent_canonical_id() {
        let mgr = AliasManager::new();
        assert!(mgr.canonical_id("不存在").is_none());
    }

    // ── 使用量与合并建议 ──

    /// 加载使用计数
    #[test]
    fn load_use_counts() {
        let mut mgr = AliasManager::new();
        let mut counts = HashMap::new();
        counts.insert("工作压力".into(), 15u32);
        counts.insert("职场焦虑".into(), 8u32);
        mgr.load_use_counts(counts);
        assert_eq!(mgr.use_count("工作压力"), 15);
        assert_eq!(mgr.use_count("职场焦虑"), 8);
        assert_eq!(mgr.use_count("不存在的"), 0);
    }

    /// 无使用计数时合并建议为空
    #[test]
    fn empty_use_counts_no_suggestions() {
        let mgr = AliasManager::new();
        let suggestions = mgr.suggest_merges(3);
        assert!(suggestions.is_empty());
    }

    /// 低使用量关键词不产生合并建议
    #[test]
    fn low_use_no_suggestion() {
        let mut mgr = AliasManager::new();
        let mut counts = HashMap::new();
        counts.insert("a".into(), 1u32);
        counts.insert("b".into(), 2u32);
        mgr.load_use_counts(counts);
        let suggestions = mgr.suggest_merges(5); // 最小阈值 5，所有词低于此
        assert!(suggestions.is_empty());
    }

    /// 相似关键词产生合并建议
    #[test]
    fn similar_keywords_get_suggestion() {
        let mut mgr = AliasManager::new();
        let mut counts = HashMap::new();
        counts.insert("工作压力".into(), 20u32);
        counts.insert("工作负担".into(), 10u32);
        mgr.load_use_counts(counts);
        let suggestions = mgr.suggest_merges(3);
        // "工作压力"和"工作负担"共享"工作"+"负"+"担"+"压"+"力"
        // 编辑距离较大但共享中文词组，应能检测相似
        // 由于中文编辑距离按字符计算，共享多个汉字 => 命中条件 1（共享 ≥2 个汉字）
        assert!(!suggestions.is_empty(), "相似关键词应产生合并建议");
        // 建议中应包含使用量高的"工作压力"作为规范词
        assert_eq!(suggestions[0].canonical_text, "工作压力");
    }

    /// 别名使用量高于规范词时建议反转
    #[test]
    fn alias_higher_usage_suggests_reverse() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("流行词", 1, "规范词").unwrap();
        let mut counts = HashMap::new();
        counts.insert("流行词".into(), 50u32); // 别名使用量高
        counts.insert("规范词".into(), 3u32); // 规范词使用量低
        mgr.load_use_counts(counts);
        let suggestions = mgr.suggest_merges(3);
        assert!(!suggestions.is_empty());
        // 建议应将"流行词"提为新的规范词
        assert!(suggestions[0].reason.contains("使用量高于当前规范词"));
    }

    // ── 清空缓存 ──

    /// clear 重置所有状态
    #[test]
    fn clear_resets_state() {
        let mut mgr = AliasManager::new();
        mgr.register_alias("a", 1, "b").unwrap();
        let mut counts = HashMap::new();
        counts.insert("c".into(), 5u32);
        mgr.load_use_counts(counts);
        mgr.clear();
        assert_eq!(mgr.alias_count(), 0);
        assert_eq!(mgr.canonical_count(), 0);
        assert_eq!(mgr.use_count("c"), 0);
    }

    // ── 辅助函数测试 ──

    /// levenshtein_distance 各输入参数化验证。
    #[test]
    fn test_levenshtein_cases() {
        let cases = [
            ("abc", "abc", 0),  // 相同
            ("abc", "abcd", 1), // 插入
            ("abcd", "abc", 1), // 删除
            ("abc", "abd", 1),  // 替换
        ];
        for (a, b, expected) in cases {
            assert_eq!(levenshtein_distance(a, b), expected, "{a} vs {b}");
        }
    }

    /// is_similar_keyword 各输入参数化验证。
    #[test]
    fn test_is_similar_cases() {
        let cases = [
            ("same", "same", false),            // 相同词不算相似
            ("工作压力", "工作焦虑", true),     // 共享"工""作"
            ("职场工作压力", "工作压力", true), // 包含关系
            ("abc", "xyz", false),              // 无共同字
        ];
        for (a, b, expected) in cases {
            assert_eq!(is_similar_keyword(a, b), expected, "{a} vs {b}");
        }
    }
}
