//! crates/ramaria-memory/src/job/tests.rs - //! crates/ramaria-memory/src/job.rs - Ramaria 后台任务管理器单元测试
//!
//! 设计特点:
//! - 位于 job 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 job.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, Persona,
    PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};
use uuid::Uuid;

/// JobType::as_str 与 Display（Display 委托 as_str）。
#[test]
fn test_job_type_str_and_display() {
    let cases = [
        (JobType::L1Summary, "l1_summary"),
        (JobType::L1SummaryRetry, "l1_summary_retry"),
        (JobType::EventExtract, "event_extract"),
        (JobType::PersonalityInference, "personality_inference"),
        (JobType::Calibration, "calibration"),
        (JobType::IndexRebuild, "index_rebuild"),
        (JobType::Custom("custom_task"), "custom_task"),
    ];
    for (job, expected) in cases {
        assert_eq!(job.as_str(), expected);
        assert_eq!(format!("{job}"), expected);
    }
    assert_eq!(format!("{}", JobType::Custom("adhoc")), "adhoc");
}

#[test]
fn test_status_constants() {
    assert_eq!(status::PENDING, "pending");
    assert_eq!(status::RUNNING, "running");
    assert_eq!(status::COMPLETED, "completed");
    assert_eq!(status::FAILED, "failed");
    assert_eq!(status::RETRYING, "retrying");
    assert_eq!(status::FATAL, "fatal");
}

/// JobManagerConfig 默认值与自定义构造验证。
#[test]
fn test_job_manager_config_cases() {
    let default = JobManagerConfig::default();
    assert_eq!(default.max_retries, 3);
    assert_eq!(default.retry_base_delay_ms, 1000);
    let custom = JobManagerConfig {
        max_retries: 5,
        retry_base_delay_ms: 2000,
    };
    assert_eq!(custom.max_retries, 5);
    assert_eq!(custom.retry_base_delay_ms, 2000);
}

/// CancellationToken 取消状态与 clone 共享状态验证。
#[test]
fn test_cancellation_token_cases() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());
    let clone = token.clone();
    token.cancel();
    assert!(token.is_cancelled());
    assert!(clone.is_cancelled(), "clone 应共享同一取消状态");
}

// =========================================================
// execute_with_retry 单元测试（mock 存储记录状态迁移）
// =========================================================

/// 记录 background_job 状态迁移的内存 mock。
///
/// 仅实现 job 生命周期需要的两个方法，其余方法 unreachable（unimplemented!）。
struct JobMockStorage {
    next_id: std::sync::atomic::AtomicI64,
    transitions: std::sync::Mutex<Vec<(i64, String)>>,
}

impl JobMockStorage {
    fn new() -> Self {
        Self {
            next_id: std::sync::atomic::AtomicI64::new(1),
            transitions: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// 已记录的状态迁移（按时间顺序）。
    fn transitions(&self) -> Vec<(i64, String)> {
        self.transitions.lock().unwrap().clone()
    }

    /// 指定状态的出现次数（如 retrying = 退避等待次数）。
    fn count_status(&self, status: &str) -> usize {
        self.transitions()
            .iter()
            .filter(|(_, s)| s == status)
            .count()
    }
}

#[async_trait::async_trait]
impl ramaria_core::StoreCrud for JobMockStorage {
    async fn create_session(&self, _persona_uid: Option<&str>) -> RamariaResult<Session> {
        unimplemented!()
    }
    async fn close_session(&self, _session_id: Uuid) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_session(&self, _session_id: Uuid) -> RamariaResult<Option<Session>> {
        unimplemented!()
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        unimplemented!()
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        unimplemented!()
    }
    async fn delete_session(&self, _session_id: Uuid) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn save_message(&self, _message: &Message) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_messages(&self, _session_id: Uuid) -> RamariaResult<Vec<Message>> {
        unimplemented!()
    }
    async fn list_messages_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<Message>> {
        unimplemented!()
    }
    async fn save_memory_l1(&self, _memory: &MemoryL1) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        unimplemented!()
    }
    async fn get_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        unimplemented!()
    }
    async fn mark_l1_absorbed(&self, _ids: &[Uuid]) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_unabsorbed_l1(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        unimplemented!()
    }
    async fn create_persona(&self, _persona: &Persona) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn get_persona_by_uid(&self, _uid: &str) -> RamariaResult<Option<Persona>> {
        unimplemented!()
    }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        unimplemented!()
    }
    async fn update_persona(
        &self,
        _uid: &str,
        _name: &str,
        _kind: Option<&str>,
        _base_prompt: Option<&str>,
        _style: Option<&str>,
    ) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn save_event(&self, _event: &MemoryEvent) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_events_by_persona(
        &self,
        _persona_uid: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        unimplemented!()
    }
    async fn list_unabsorbed_events(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        unimplemented!()
    }
    async fn mark_events_absorbed(&self, _ids: &[i64]) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn save_event_relation(&self, _relation: &EventRelation) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn save_event_source(
        &self,
        _event_id: i64,
        _l1_id: Uuid,
        _weight: f64,
    ) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn save_fact(&self, _fact: &PersonaFact) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_facts_by_persona(
        &self,
        _persona_uid: &str,
        _field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        unimplemented!()
    }
    async fn save_trait(&self, _trait: &PersonalityTrait) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_traits_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        unimplemented!()
    }
    async fn update_trait_confidence(
        &self,
        _trait_id: i64,
        _conf: f64,
        _e_total: f64,
        _consistency: f64,
    ) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn update_trait_status(&self, _trait_id: i64, _status: TraitStatus) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn save_evidence(&self, _evidence: &TraitEvidence) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_evidence_by_trait(&self, _trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        unimplemented!()
    }
    async fn save_example(&self, _example: &PersonaExample) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn list_selected_examples(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        unimplemented!()
    }
    async fn save_cluster_snapshot(&self, _snapshot: &ClusterSnapshot) -> RamariaResult<i64> {
        unimplemented!()
    }
    async fn get_current_snapshots(
        &self,
        _persona_uid: &str,
        _scope: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        unimplemented!()
    }
    async fn upsert_keyword(&self, _keyword: &str) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        unimplemented!()
    }
}

#[async_trait::async_trait]
impl ramaria_core::StoreInfrastructure for JobMockStorage {
    async fn create_background_job(
        &self,
        _job_type: &str,
        _payload: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }

    async fn update_job_status(
        &self,
        job_id: i64,
        status: &str,
        _error_msg: Option<&str>,
    ) -> RamariaResult<()> {
        self.transitions
            .lock()
            .unwrap()
            .push((job_id, status.to_string()));
        Ok(())
    }

    async fn save_privacy_consent(&self, _consent: &PrivacyConsent) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_privacy_consent(
        &self,
        _persona_uid: &str,
        _field: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        unimplemented!()
    }
    async fn save_backend_config(&self, _config: &BackendConfig) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        unimplemented!()
    }
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        unimplemented!()
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        unimplemented!()
    }
    async fn set_index_version(&self, _version: i32) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        unimplemented!()
    }
    async fn get_setting(&self, _key: &str) -> RamariaResult<Option<String>> {
        unimplemented!()
    }
    async fn set_setting(&self, _key: &str, _value: &str) -> RamariaResult<()> {
        unimplemented!()
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        unimplemented!()
    }
    async fn insert_keyword_ref(
        &self,
        _keyword_id: &str,
        _doc_type: &str,
        _doc_id: &str,
        _persona_uid: &str,
        _weight: f64,
    ) -> RamariaResult<()> {
        unimplemented!()
    }
}

/// 核心重试：可重试失败 N 次后成功 → 任务最终 completed。
#[tokio::test]
async fn execute_with_retry_retries_then_succeeds() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let storage = JobMockStorage::new();
    let manager = JobManager::new(
        &storage,
        JobManagerConfig {
            max_retries: 3,
            retry_base_delay_ms: 1, // 退避 1ms/2ms，测试快速完成
        },
    );

    let calls = AtomicUsize::new(0);
    let job_id = manager
        .execute_with_retry(JobType::IndexRebuild, None, None, || {
            let calls = &calls;
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    JobResult::Retryable(format!("第 {} 次失败", n + 1))
                } else {
                    JobResult::Success
                }
            }
        })
        .await
        .expect("重试后应成功");
    assert_eq!(job_id, 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "应执行 3 次（2 次失败 + 1 次成功）"
    );
    // 状态迁移：running → retrying → running → retrying → running → completed
    assert_eq!(storage.count_status(status::COMPLETED), 1);
    assert_eq!(
        storage.count_status(status::RETRYING),
        2,
        "2 次可重试失败 → 2 次退避等待"
    );
    assert_eq!(storage.count_status(status::FAILED), 0);
}

/// 非可重试错误（Fatal）不重试，直接 failed。
#[tokio::test]
async fn execute_with_retry_fatal_does_not_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let storage = JobMockStorage::new();
    let manager = JobManager::new(
        &storage,
        JobManagerConfig {
            max_retries: 3,
            retry_base_delay_ms: 1,
        },
    );

    let calls = AtomicUsize::new(0);
    let err = manager
        .execute_with_retry(JobType::IndexRebuild, None, None, || {
            let calls = &calls;
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                JobResult::Fatal("存储不可用".to_string())
            }
        })
        .await
        .expect_err("Fatal 错误应返回 Err");
    assert!(
        err.to_string().contains("致命错误"),
        "错误信息应标注致命: {err}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "Fatal 不重试，只执行 1 次");
    assert_eq!(storage.count_status(status::FAILED), 1);
    assert_eq!(storage.count_status(status::RETRYING), 0, "Fatal 不应退避");
    assert_eq!(storage.count_status(status::COMPLETED), 0);
}

/// 可重试失败达到 max_retries → failed，退避次数 = max_retries - 1。
#[tokio::test]
async fn execute_with_retry_exhausts_retries_marks_failed() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let storage = JobMockStorage::new();
    let manager = JobManager::new(
        &storage,
        JobManagerConfig {
            max_retries: 2,
            retry_base_delay_ms: 1,
        },
    );

    let calls = AtomicUsize::new(0);
    let err = manager
        .execute_with_retry(JobType::IndexRebuild, None, None, || {
            let calls = &calls;
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                JobResult::Retryable("一直失败".to_string())
            }
        })
        .await
        .expect_err("达到最大重试次数应返回 Err");
    assert!(
        err.to_string().contains("已达最大重试次数"),
        "错误信息应标注达到上限: {err}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "执行 2 次（max_retries=2）"
    );
    assert_eq!(storage.count_status(status::FAILED), 1);
    assert_eq!(
        storage.count_status(status::RETRYING),
        1,
        "2 次尝试之间只有 1 次退避"
    );
    assert_eq!(storage.count_status(status::COMPLETED), 0);
}
