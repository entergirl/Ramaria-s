//! crates/ramaria-memory/src/keyword/service/fuzzy.rs - 语义扩展层维护
//!
//! 设计特点:
//! - 构建在锁外异步完成，结果经 set_fuzzy 挂载，避免写锁跨 await
//! - embedding 不可用 / 词表为空 / 构建失败均返回 None，保持精确 + 子串两层降级
//! - 陈旧判据按词数差近似，告警按时间窗节流

use super::*;

impl KeywordService {
    // =========================================================
    // 语义层（Fuzzy）
    // =========================================================

    /// 直接挂载语义扩展层（构建由调用方在锁外异步完成后写入，避免写锁跨 await）。
    pub fn set_fuzzy(&mut self, fuzzy: Option<FuzzyKeywordIndex>) {
        Arc::make_mut(&mut self.composite).set_fuzzy(fuzzy);
    }

    /// 用词表构建语义扩展层（锁外 await 完成后由调用方经 `set_fuzzy` 挂载）。
    ///
    /// 参数:
    /// - `terms`: 词表（通常为已确认词表 token，由调用方在锁内取出后释放锁）。
    /// - `embedder`: 向量生成器；`None` 表示 embedding 不可用。
    ///
    /// 返回:
    /// - `Some(FuzzyKeywordIndex)`: 构建成功的语义层。
    /// - `None`: 降级（保持"精确 + 子串"两层，不抛错）。
    ///
    /// 降级语义（静默降级，不抛错）:
    /// - embedder 为 None → None（debug 日志）
    /// - 词表为空 → None（debug 日志）
    /// - 构建失败 → None（warn 日志，不含词条文本）
    ///
    /// 说明:
    /// - 无 `&self` 参数：可在 std 写锁外 await，避免锁跨 await；
    ///   构建结果由调用方在锁内经 `set_fuzzy` 挂载。
    pub async fn build_fuzzy(
        terms: &[KeywordToken],
        embedder: Option<&dyn EmbeddingProvider>,
    ) -> Option<FuzzyKeywordIndex> {
        let Some(embedder) = embedder else {
            tracing::debug!("关键词语义层跳过：embedding 不可用（两层降级）");
            return None;
        };
        if terms.is_empty() {
            tracing::debug!("关键词语义层跳过：词表为空（无可用词条）");
            return None;
        }
        match FuzzyKeywordIndex::build(terms, embedder).await {
            Ok(fuzzy) => {
                tracing::info!(entry_count = terms.len(), "关键词语义层构建完成");
                Some(fuzzy)
            }
            Err(e) => {
                tracing::warn!(error = %e, "关键词语义层构建失败（两层降级）");
                None
            }
        }
    }

    /// 语义层是否陈旧（以词数差为近似判据）。
    ///
    /// 判据:
    /// - `composite.fuzzy()` 为 None 且已确认词表非空 → true（未构建）。
    /// - `Some(f)` 且 `f.len()` 与已确认词表词数不一致 → true（词表已变化）。
    /// - 否则 false。
    ///
    /// 说明:
    /// - 词数一致但词表内容有增删替换时本判据发现不了——以词数差近似，
    ///   避免逐词比对向查询路径引入额外开销；精确判据应由调用方携带词表版本自行维护。
    pub fn fuzzy_stale(&self) -> bool {
        let established_len = self.pool.established_terms().len();
        match self.composite.fuzzy() {
            None => established_len > 0,
            Some(fuzzy) => fuzzy.len() != established_len,
        }
    }

    /// 语义层陈旧时按节流打 warn 并返回 true；不陈旧返回 false。
    ///
    /// 参数:
    /// - `now_ms`: 当前时间（Unix 毫秒；节流窗口以调用方时间为准）。
    ///
    /// 返回:
    /// - `true`: 当前陈旧（本次是否实际打日志由节流决定）。
    /// - `false`: 不陈旧。
    ///
    /// 说明:
    /// - 节流窗口 5 分钟：窗口内重复调用不再打日志，避免按请求频次刷屏。
    /// - 日志只记录池词表词数与语义层词数（不含词条文本）。
    pub fn warn_if_fuzzy_stale(&mut self, now_ms: i64) -> bool {
        if !self.fuzzy_stale() {
            return false;
        }
        let should_log = match self.fuzzy_stale_warned_at {
            Some(last) => now_ms.saturating_sub(last) >= FUZZY_STALE_WARN_INTERVAL_MS,
            None => true,
        };
        if should_log {
            self.fuzzy_stale_warned_at = Some(now_ms);
            tracing::warn!(
                pool_terms = self.pool.established_terms().len(),
                fuzzy_terms = self.composite.fuzzy().map(|f| f.len()).unwrap_or(0),
                "关键词语义层陈旧：词表已变化但语义扩展层未重建（保持两层降级可用）"
            );
        }
        true
    }
}
