//! crates/ramaria-testkit/src/mock/storage/utt.rs - MockStorage 的原文话语块域实现
//!
//! 设计特点:
//! - 话语块按会话分组，id 在会话内自增
//! - persona 查询跨会话聚合并按块 id 升序
//! - 最新块取会话列表尾部，删除按单块 / 整会话两种粒度

use ramaria_core::error::RamariaResult;
use ramaria_core::types::UttBlock;
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn insert_utt_block(&self, block: &UttBlock) -> RamariaResult<i64> {
        self.guard("insert_utt_block")?;
        let mut map = self.utt_blocks.lock().expect("utt 锁");
        let list = map.entry(block.session_id).or_default();
        let id = list.len() as i64 + 1;
        let mut stored = block.clone();
        stored.id = id;
        list.push(stored);
        Ok(id)
    }

    pub(crate) async fn list_utt_blocks_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<UttBlock>> {
        self.guard("list_utt_blocks_by_persona")?;
        let mut blocks: Vec<UttBlock> = self
            .utt_blocks
            .lock()
            .expect("utt 锁")
            .values()
            .flatten()
            .filter(|block| block.persona_uid == persona_uid)
            .cloned()
            .collect();
        blocks.sort_by_key(|block| block.id);
        Ok(blocks)
    }

    pub(crate) async fn get_latest_utt_block_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<UttBlock>> {
        self.guard("get_latest_utt_block_by_session")?;
        Ok(self
            .utt_blocks
            .lock()
            .expect("utt 锁")
            .get(&session_id)
            .and_then(|list| list.last())
            .cloned())
    }

    pub(crate) async fn delete_utt_block(&self, id: i64) -> RamariaResult<()> {
        self.guard("delete_utt_block")?;
        for list in self.utt_blocks.lock().expect("utt 锁").values_mut() {
            list.retain(|block| block.id != id);
        }
        Ok(())
    }

    pub(crate) async fn delete_utt_blocks_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<usize> {
        self.guard("delete_utt_blocks_by_session")?;
        Ok(self
            .utt_blocks
            .lock()
            .expect("utt 锁")
            .remove(&session_id)
            .map(|list| list.len())
            .unwrap_or(0))
    }
}
