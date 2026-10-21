//! crates/ramaria-testkit/src/mock/storage/infra.rs - MockStorage 的关键词与表情资产域实现
//!
//! 设计特点:
//! - 关键词词典与词池快照分离：词典供写侧去重，词池供状态查询
//! - 词池读取计次，供每会话一次快照的开销口径断言
//! - 待确认别名登记幂等：同文本重复登记返回 false 且不覆盖
//! - 表情资产按 id 自增存储，标签覆盖写入

use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::{KeywordPoolRow, PendingAliasRow};
use ramaria_core::types::StickerAsset;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> {
        self.guard("upsert_keyword")?;
        let mut keywords = self.keywords.lock().expect("关键词锁");
        if !keywords.iter().any(|existing| existing == keyword) {
            keywords.push(keyword.to_string());
        }
        Ok(())
    }

    pub(crate) async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        self.guard("list_keywords")?;
        Ok(self.keywords.lock().expect("关键词锁").clone())
    }

    pub(crate) async fn seed_keyword_canonical(&self, keyword: &str) -> RamariaResult<bool> {
        self.guard("seed_keyword_canonical")?;
        let mut keywords = self.keywords.lock().expect("关键词锁");
        if keywords.iter().any(|existing| existing == keyword) {
            return Ok(false);
        }
        keywords.push(keyword.to_string());
        let mut rows = self.keyword_pool_rows.lock().expect("词池锁");
        let rowid = rows.len() as i64 + 1;
        rows.push(KeywordPoolRow {
            rowid,
            keyword: keyword.to_string(),
            use_count: 0,
            created_at: 1000,
            alias_status: None,
            canonical_id: None,
            canonical_keyword: None,
        });
        Ok(true)
    }

    pub(crate) async fn list_canonical_keywords(&self) -> RamariaResult<Vec<String>> {
        self.guard("list_canonical_keywords")?;
        let rows = self.keyword_pool_rows.lock().expect("词池锁");
        if rows.is_empty() {
            return Ok(self.keywords.lock().expect("关键词锁").clone());
        }
        Ok(rows
            .iter()
            .filter(|row| row.alias_status.as_deref().is_none_or(|s| s == "canonical"))
            .map(|row| row.keyword.clone())
            .collect())
    }

    pub(crate) async fn list_established_keywords(&self) -> RamariaResult<Vec<String>> {
        self.guard("list_established_keywords")?;
        let rows = self.keyword_pool_rows.lock().expect("词池锁");
        if rows.is_empty() {
            return Ok(self.keywords.lock().expect("关键词锁").clone());
        }
        Ok(rows
            .iter()
            .filter(|row| row.alias_status.as_deref() != Some("pending"))
            .map(|row| row.keyword.clone())
            .collect())
    }

    pub(crate) async fn list_keyword_pool_entries(&self) -> RamariaResult<Vec<KeywordPoolRow>> {
        self.guard("list_keyword_pool_entries")?;
        self.pool_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.keyword_pool_rows.lock().expect("词池锁").clone())
    }

    pub(crate) async fn list_keyword_statuses(
        &self,
        keywords: &[String],
    ) -> RamariaResult<Vec<(String, Option<String>)>> {
        self.guard("list_keyword_statuses")?;
        let rows = self.keyword_pool_rows.lock().expect("词池锁");
        Ok(keywords
            .iter()
            .filter_map(|keyword| {
                rows.iter()
                    .find(|row| &row.keyword == keyword)
                    .map(|row| (keyword.clone(), row.alias_status.clone()))
            })
            .collect())
    }

    pub(crate) async fn list_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasRow>> {
        self.guard("list_pending_aliases")?;
        let writes = self.pending_alias_writes.lock().expect("别名登记锁");
        let rows = self.keyword_pool_rows.lock().expect("词池锁");
        Ok(writes
            .iter()
            .map(|(alias, canonical_id)| {
                let alias_id = rows
                    .iter()
                    .find(|row| &row.keyword == alias)
                    .map(|row| row.rowid)
                    .unwrap_or(0);
                let canonical_keyword = rows
                    .iter()
                    .find(|row| row.rowid == *canonical_id)
                    .map(|row| row.keyword.clone())
                    .unwrap_or_default();
                PendingAliasRow {
                    alias_id,
                    alias_keyword: alias.clone(),
                    canonical_id: *canonical_id,
                    canonical_keyword,
                    created_at: 1000,
                }
            })
            .collect())
    }

    pub(crate) async fn confirm_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        self.guard("confirm_keyword_alias")?;
        let mut rows = self.keyword_pool_rows.lock().expect("词池锁");
        match rows.iter_mut().find(|row| row.rowid == alias_id) {
            Some(row)
                if row.alias_status.as_deref() == Some("pending") && row.canonical_id.is_some() =>
            {
                row.alias_status = Some("alias".to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(crate) async fn reject_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        self.guard("reject_keyword_alias")?;
        let mut rows = self.keyword_pool_rows.lock().expect("词池锁");
        match rows.iter_mut().find(|row| row.rowid == alias_id) {
            Some(row) if row.alias_status.as_deref() == Some("pending") => {
                row.alias_status = None;
                row.canonical_id = None;
                row.canonical_keyword = None;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(crate) async fn upsert_pending_alias(
        &self,
        alias: &str,
        canonical_id: i64,
        use_count: u32,
    ) -> RamariaResult<bool> {
        self.guard("upsert_pending_alias")?;
        let mut writes = self.pending_alias_writes.lock().expect("别名登记锁");
        if writes.iter().any(|(text, _)| text == alias) {
            return Ok(false);
        }
        let mut rows = self.keyword_pool_rows.lock().expect("词池锁");
        if rows.iter().any(|row| row.keyword == alias) {
            return Ok(false);
        }
        let rowid = rows.len() as i64 + 1;
        let canonical_keyword = rows
            .iter()
            .find(|row| row.rowid == canonical_id)
            .map(|row| row.keyword.clone());
        rows.push(KeywordPoolRow {
            rowid,
            keyword: alias.to_string(),
            use_count: use_count as i64,
            created_at: 1000,
            alias_status: Some("pending".to_string()),
            canonical_id: Some(canonical_id),
            canonical_keyword,
        });
        writes.push((alias.to_string(), canonical_id));
        Ok(true)
    }

    pub(crate) async fn insert_sticker_asset(&self, asset: &StickerAsset) -> RamariaResult<i64> {
        self.guard("insert_sticker_asset")?;
        let id = self
            .sticker_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = asset.clone();
        stored.id = id;
        self.stickers.lock().expect("表情锁").insert(id, stored);
        Ok(id)
    }

    pub(crate) async fn list_sticker_assets(&self) -> RamariaResult<Vec<StickerAsset>> {
        self.guard("list_sticker_assets")?;
        let stickers = self.stickers.lock().expect("表情锁");
        let mut list: Vec<StickerAsset> = stickers.values().cloned().collect();
        list.sort_by_key(|asset| asset.id);
        Ok(list)
    }

    pub(crate) async fn get_sticker_asset(&self, id: i64) -> RamariaResult<Option<StickerAsset>> {
        self.guard("get_sticker_asset")?;
        Ok(self.stickers.lock().expect("表情锁").get(&id).cloned())
    }

    pub(crate) async fn update_sticker_asset(
        &self,
        id: i64,
        description: Option<&str>,
        tags: Option<&[String]>,
    ) -> RamariaResult<bool> {
        self.guard("update_sticker_asset")?;
        match self.stickers.lock().expect("表情锁").get_mut(&id) {
            Some(asset) => {
                if let Some(description) = description {
                    asset.description = Some(description.to_string());
                }
                if let Some(tags) = tags {
                    asset.tags = tags.to_vec();
                }
                asset.updated_at = 2000;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) async fn delete_sticker_asset(&self, id: i64) -> RamariaResult<bool> {
        self.guard("delete_sticker_asset")?;
        Ok(self.stickers.lock().expect("表情锁").remove(&id).is_some())
    }
}
