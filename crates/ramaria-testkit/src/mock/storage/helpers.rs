//! crates/ramaria-testkit/src/mock/storage/helpers.rs - MockStorage 的失败注入与造数辅助
//!
//! 设计特点:
//! - 失败注入统一守卫：未设置注入时仅两次原子读
//! - 造数辅助覆盖会话 / 消息 / L1 / 事件 / 事实 / 性格 / 附件 / 词池 / 聚合预设
//! - 全部方法 pub，供各 crate 测试按需预填数据

use std::sync::atomic::Ordering;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::keyword::KeywordPoolRow;
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, MemoryEvent, MemoryL1, Message, MessageAttachment, Persona,
    PersonaEventAggregate, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, Session,
    TraitEvidence, UttBlock,
};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    // =========================================================
    // 失败注入
    // =========================================================

    /// 指定方法持续失败（直至 clear_failures）。
    pub fn fail_on(&self, method: &str) {
        if self
            .failures
            .lock()
            .expect("失败集合锁")
            .insert(method.to_string())
        {
            self.injection_count.fetch_add(1, Ordering::Release);
        }
    }

    /// 指定方法仅失败一次。
    pub fn fail_next(&self, method: &str) {
        if self
            .fail_once
            .lock()
            .expect("失败集合锁")
            .insert(method.to_string())
        {
            self.injection_count.fetch_add(1, Ordering::Release);
        }
    }

    /// 清空全部失败注入。
    pub fn clear_failures(&self) {
        self.failures.lock().expect("失败集合锁").clear();
        self.fail_once.lock().expect("失败集合锁").clear();
        self.fail_all.store(false, Ordering::Release);
        self.injection_count.store(0, Ordering::Release);
    }

    /// 切换全部方法失败（true = 所有方法返回存储错误）。
    pub fn set_fail_all(&self, fail: bool) {
        self.fail_all.store(fail, Ordering::Release);
        if fail {
            self.injection_count.fetch_add(1, Ordering::Release);
        } else {
            self.injection_count.store(0, Ordering::Release);
        }
    }

    /// 失败注入守卫（未设置注入时仅两次原子读）。
    pub(crate) fn guard(&self, method: &str) -> RamariaResult<()> {
        if !self.fail_all.load(Ordering::Acquire)
            && self.injection_count.load(Ordering::Acquire) == 0
        {
            return Ok(());
        }
        if self.fail_all.load(Ordering::Acquire) {
            return Err(RamariaError::storage(format!(
                "MockStorage::{method} 注入失败"
            )));
        }
        let once = self.fail_once.lock().expect("失败集合锁").remove(method);
        if once {
            return Err(RamariaError::storage(format!(
                "MockStorage::{method} 注入失败"
            )));
        }
        if self.failures.lock().expect("失败集合锁").contains(method) {
            return Err(RamariaError::storage(format!(
                "MockStorage::{method} 注入失败"
            )));
        }
        Ok(())
    }

    // =========================================================
    // 便捷造数
    // =========================================================

    /// 登记一个 pending 后台任务。
    pub fn add_pending_job(&self, job_type: &str, payload: Option<&str>) -> i64 {
        let id = self.job_seq.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().expect("任务锁").insert(
            id,
            (
                job_type.to_string(),
                payload.map(|p| p.to_string()),
                "pending".to_string(),
            ),
        );
        id
    }

    /// 读取任务状态（None = 任务不存在）。
    pub fn job_status(&self, job_id: i64) -> Option<String> {
        self.jobs
            .lock()
            .expect("任务锁")
            .get(&job_id)
            .map(|(_, _, status)| status.clone())
    }

    /// 创建会话并预填充消息。
    pub fn create_session_with_messages(&self, session_id: Uuid, messages: Vec<Message>) {
        self.sessions.lock().expect("会话锁").insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: None,
                persona_uid: None,
                ..Session::default()
            },
        );
        self.messages
            .lock()
            .expect("消息锁")
            .insert(session_id, messages);
    }

    /// 创建已结束会话。
    pub fn create_ended_session(&self, session_id: Uuid) {
        self.sessions.lock().expect("会话锁").insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: Some(2000),
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 添加活跃会话。
    pub fn add_active_session(&self, session_id: Uuid) {
        self.sessions.lock().expect("会话锁").insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: None,
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 添加已关闭会话（结束时间固定 2000）。
    pub fn add_closed_session(&self, session_id: Uuid) {
        self.sessions.lock().expect("会话锁").insert(
            session_id,
            Session {
                id: session_id,
                started_at: 1000,
                ended_at: Some(2000),
                persona_uid: None,
                ..Session::default()
            },
        );
    }

    /// 追加消息。
    pub fn add_messages(&self, session_id: Uuid, msgs: Vec<Message>) {
        self.messages
            .lock()
            .expect("消息锁")
            .insert(session_id, msgs);
    }

    /// 添加一条 L1 摘要（同步维护会话与 persona 维度索引）。
    pub fn add_l1(&self, session_id: Uuid, memory: MemoryL1) {
        if let Some(uid) = memory.persona_uid.clone() {
            self.l1_by_persona
                .lock()
                .expect("L1 索引锁")
                .entry(uid)
                .or_default()
                .push(memory.clone());
        }
        self.l1_list
            .lock()
            .expect("L1 列表锁")
            .entry(session_id)
            .or_default()
            .push(memory);
    }

    /// 按 persona 添加 L1 摘要（替换该 persona 的索引列表）。
    pub fn add_l1_summaries(&self, persona_uid: &str, summaries: Vec<MemoryL1>) {
        self.l1_by_persona
            .lock()
            .expect("L1 索引锁")
            .insert(persona_uid.to_string(), summaries);
    }

    /// 添加 persona。
    pub fn add_persona(&self, persona: Persona) {
        self.personas
            .lock()
            .expect("人格锁")
            .insert(persona.uid.clone(), persona);
    }

    /// 注入一条 Few-shot 示例（selected 可控）。
    pub fn add_example(&self, persona_uid: &str, example: PersonaExample) {
        self.examples
            .lock()
            .expect("示例锁")
            .entry(persona_uid.to_string())
            .or_default()
            .push(example);
    }

    /// 为指定会话追加一条 utt 话语块（id 按会话内块数分配）。
    pub fn add_utt_block(&self, mut block: UttBlock) {
        let mut map = self.utt_blocks.lock().expect("utt 锁");
        let list = map.entry(block.session_id).or_default();
        block.id = list.len() as i64 + 1;
        list.push(block);
    }

    /// 添加隐私确认。
    pub fn add_privacy_consent(&self, consent: PrivacyConsent) {
        self.privacy_consents
            .lock()
            .expect("隐私确认锁")
            .push(consent);
    }

    /// 设置后端配置。
    pub fn set_backend_config(&self, config: BackendConfig) {
        *self.backend_config.lock().expect("后端配置锁") = Some(config);
    }

    /// 添加设置项。
    pub fn add_setting(&self, key: &str, value: &str) {
        self.settings
            .lock()
            .expect("设置锁")
            .insert(key.to_string(), value.to_string());
    }

    /// 添加 L2 事件（自动分配 id 并建立 persona 索引）。
    pub fn add_event(&self, persona_uid: &str, mut event: MemoryEvent) -> i64 {
        let id = self.event_seq.fetch_add(1, Ordering::SeqCst);
        event.id = id;
        event.persona_uid = persona_uid.to_string();
        self.events.lock().expect("事件锁").insert(id, event);
        self.events_by_persona
            .lock()
            .expect("事件索引锁")
            .entry(persona_uid.to_string())
            .or_default()
            .push(id);
        id
    }

    /// 添加 PersonalityTrait 记录（自动分配 id 并建立 persona 索引）。
    pub fn add_personality_trait(&self, persona_uid: &str, mut record: PersonalityTrait) -> i64 {
        let id = self.trait_seq.fetch_add(1, Ordering::SeqCst);
        record.id = id;
        record.persona_uid = persona_uid.to_string();
        self.traits.lock().expect("性格锁").insert(id, record);
        self.traits_by_persona
            .lock()
            .expect("性格索引锁")
            .entry(persona_uid.to_string())
            .or_default()
            .push(id);
        id
    }

    /// 添加 PersonalityTrait 记录（自动分配 id）。
    pub fn add_trait(&self, t: PersonalityTrait) -> i64 {
        let uid = t.persona_uid.clone();
        self.add_personality_trait(&uid, t)
    }

    /// 添加 TraitEvidence 记录（自动分配 id）。
    pub fn add_evidence(&self, mut e: TraitEvidence) -> i64 {
        let id = self.evidence_seq.fetch_add(1, Ordering::SeqCst);
        e.id = id;
        self.evidence
            .lock()
            .expect("证据锁")
            .entry(e.trait_id)
            .or_default()
            .push(e);
        id
    }

    /// 添加 ClusterSnapshot 记录（自动分配 id）。
    pub fn add_cluster_snapshot(&self, mut s: ClusterSnapshot) -> i64 {
        let id = self.snapshot_seq.fetch_add(1, Ordering::SeqCst);
        s.id = id;
        self.cluster_snapshots.lock().expect("快照锁").push(s);
        id
    }

    /// 已存储的 trait 数量。
    pub fn trait_count(&self) -> usize {
        self.traits.lock().expect("性格锁").len()
    }

    /// 已存储的 evidence 数量。
    pub fn evidence_count(&self) -> usize {
        self.evidence
            .lock()
            .expect("证据锁")
            .values()
            .map(|v| v.len())
            .sum()
    }

    /// 注入一条 PersonaFact（自动分配 id 并建立 persona 索引）。
    pub fn add_fact(&self, mut f: PersonaFact) -> i64 {
        let id = self.fact_seq.fetch_add(1, Ordering::SeqCst);
        f.id = id;
        let persona = f.persona_uid.clone();
        self.facts.lock().expect("事实锁").insert(id, f);
        self.facts_by_persona
            .lock()
            .expect("事实索引锁")
            .entry(persona)
            .or_default()
            .push(id);
        id
    }

    /// 构造事实版本链覆盖：旧事实置 superseded，新事实写入并指向旧 id。
    pub fn add_fact_with_version(&self, old: &PersonaFact, mut fresh: PersonaFact) -> i64 {
        let new_id = self.fact_seq.fetch_add(1, Ordering::SeqCst);
        fresh.id = new_id;
        fresh.status = ramaria_core::types::FactStatus::Active;
        fresh.version_of = Some(old.id);
        {
            let mut facts = self.facts.lock().expect("事实锁");
            if let Some(o) = facts.get_mut(&old.id) {
                o.status = ramaria_core::types::FactStatus::Superseded;
                o.updated_at = fresh.updated_at;
            }
            facts.insert(new_id, fresh.clone());
        }
        self.facts_by_persona
            .lock()
            .expect("事实索引锁")
            .entry(fresh.persona_uid)
            .or_default()
            .push(new_id);
        new_id
    }

    /// 追加一条消息附件行（id 自动分配）。
    pub fn add_attachment(&self, mut attachment: MessageAttachment) -> i64 {
        let id = self.attachment_seq.fetch_add(1, Ordering::SeqCst);
        attachment.id = id;
        self.attachments
            .lock()
            .expect("附件锁")
            .insert(id, attachment);
        id
    }

    /// 设置关键词词典（供 list_keywords 返回）。
    pub fn set_keywords(&self, kws: Vec<String>) {
        *self.keywords.lock().expect("关键词锁") = kws;
    }

    /// 已保存的 L1 条目（用于断言）。
    pub fn saved_l1_entries(&self) -> Vec<MemoryL1> {
        self.l1_list
            .lock()
            .expect("L1 列表锁")
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// 关键词数量。
    pub fn keyword_count(&self) -> usize {
        self.keywords.lock().expect("关键词锁").len()
    }

    /// 预置词池快照。
    pub fn set_pool_rows(&self, rows: Vec<KeywordPoolRow>) {
        *self.keyword_pool_rows.lock().expect("词池锁") = rows;
    }

    /// 已登记的待确认别名 (alias, canonical_id)。
    pub fn pending_writes(&self) -> Vec<(String, i64)> {
        self.pending_alias_writes
            .lock()
            .expect("别名登记锁")
            .clone()
    }

    /// 词池快照读取次数。
    pub fn pool_read_count(&self) -> usize {
        self.pool_reads.load(Ordering::Relaxed)
    }

    /// 预设跨用户事件聚合成功返回。
    pub fn set_persona_event_aggregates(&self, rows: Vec<PersonaEventAggregate>) {
        *self.persona_event_aggregates.lock().expect("聚合锁") = Some(rows);
        *self.persona_event_aggregate_error.lock().expect("聚合锁") = None;
    }

    /// 预设跨用户事件聚合失败。
    pub fn set_persona_event_aggregate_error(&self, msg: &str) {
        *self.persona_event_aggregate_error.lock().expect("聚合锁") = Some(msg.to_string());
        *self.persona_event_aggregates.lock().expect("聚合锁") = None;
    }
}
