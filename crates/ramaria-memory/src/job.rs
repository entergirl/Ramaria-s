//! rust/crates/ramaria-memory/src/job.rs - Ramaria 后台任务管理器
//!
//! 设计特点:
//! - 封装 StorageBackend 的 background_jobs CRUD，提供类型安全的 JobType 枚举
//! - 支持 create → running → completed/failed 完整生命周期
//! - 内置重试逻辑: 最大重试次数、指数退避、错误记录
//! - 支持 CancellationToken：应用关闭时可优雅取消正在执行的任务
//! - 状态标记（mark_running/mark_retrying）失败时立即终止任务，标记为 fatal
//! - 所有操作通过 tracing 记录观测日志（info/warn/error 分级）
//! - 纯编排层，不直接访问数据库——通过 &dyn StorageBackend 注入

use ramaria_core::{RamariaError, RamariaResult, StorageBackend};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// =========================================================
// 后台任务类型枚举
// =========================================================

/// 后台任务类型。
///
/// 职责:
/// - 将自由文本 job_type 约束为已知任务类型，避免拼写错误和魔法字符串。
/// - 每种类型对应唯一的字符串标识（存入 background_jobs.job_type 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobType {
    /// L0→L1 摘要生成
    L1Summary,
    /// L1→L2 事件提取
    EventExtract,
    /// L2→L3 性格推断（+B）
    PersonalityInference,
    /// L3 画像全量校准
    Calibration,
    /// 索引重建
    IndexRebuild,
    /// 自定义任务类型（向后兼容，不推荐新增使用）
    Custom(&'static str),
}

impl JobType {
    /// 返回数据库中存储的字符串标识。
    pub fn as_str(&self) -> &str {
        match self {
            JobType::L1Summary => "l1_summary",
            JobType::EventExtract => "event_extract",
            JobType::PersonalityInference => "personality_inference",
            JobType::Calibration => "calibration",
            JobType::IndexRebuild => "index_rebuild",
            JobType::Custom(s) => s,
        }
    }
}

impl std::fmt::Display for JobType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// =========================================================
// 任务状态常量
// =========================================================

/// 后台任务状态常量（存入 background_jobs.status 列）。
pub mod status {
    /// 等待执行
    pub const PENDING: &str = "pending";
    /// 正在执行
    pub const RUNNING: &str = "running";
    /// 执行成功
    pub const COMPLETED: &str = "completed";
    /// 执行失败（已达最大重试次数）
    pub const FAILED: &str = "failed";
    /// 等待重试
    pub const RETRYING: &str = "retrying";
    /// 致命错误（存储不可用，无法继续执行）
    pub const FATAL: &str = "fatal";
}

// =========================================================
// JobManager 配置
// =========================================================

/// 后台任务管理器配置。
///
/// 字段约定:
/// - `max_retries`: 最大重试次数，默认 3。
/// - `retry_base_delay_ms`: 基础延迟毫秒数，指数退避 = base * 2^(attempt-1)。
#[derive(Debug, Clone)]
pub struct JobManagerConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 基础延迟（毫秒）
    pub retry_base_delay_ms: u64,
}

impl Default for JobManagerConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_base_delay_ms: 1000,
        }
    }
}

// =========================================================
// 任务执行结果
// =========================================================

/// 单次任务执行结果。
#[derive(Debug)]
#[non_exhaustive]
pub enum JobResult {
    /// 执行成功
    Success,
    /// 可重试的失败（含错误信息）
    Retryable(String),
    /// 不可重试的失败（含错误信息）
    Fatal(String),
}

// =========================================================
// JobManager
// =========================================================

/// 后台任务管理器。
///
/// 职责:
/// - 创建、启动、标记完成/失败任务的生命周期管理。
/// - 自动重试: `execute_with_retry` 在可重试失败时自动退避重试。
/// - 观测: 每个状态变更通过 tracing 宏记录，包含 job_id 和 job_type。
///
/// 用法:
/// ```ignore
/// let manager = JobManager::new(storage, JobManagerConfig::default);
/// let job_id = manager.create(JobType::IndexRebuild, None).await?;
/// manager.mark_running(job_id).await?;
/// // ... 执行实际工作 ...
/// manager.mark_completed(job_id).await?;
/// ```
pub struct JobManager<'a> {
    storage: &'a dyn StorageBackend,
    config: JobManagerConfig,
}

impl<'a> JobManager<'a> {
    /// 创建新任务管理器。
    pub fn new(storage: &'a dyn StorageBackend, config: JobManagerConfig) -> Self {
        Self { storage, config }
    }

    /// 使用默认配置创建任务管理器。
    pub fn with_defaults(storage: &'a dyn StorageBackend) -> Self {
        Self {
            storage,
            config: JobManagerConfig::default(),
        }
    }

    /// 将任务类型映射到合适的 RamariaError 类别。
    ///
    /// 职责:
    /// - LLM 相关任务（L1Summary、EventExtract、PersonalityInference）→ `RamariaError::llm`
    /// - 索引任务 → `RamariaError::index`
    /// - 校准和自定义任务 → `RamariaError::validation`
    ///
    /// 说明:
    /// - 之前所有错误统一包装为 `RamariaError::storage`，掩盖了实际故障来源。
    /// - 现在按任务语义分类，便于日志告警、UI 展示和调用方针对性处理。
    fn error_for_job(&self, job_type: JobType, message: impl Into<String>) -> RamariaError {
        match job_type {
            JobType::L1Summary | JobType::EventExtract | JobType::PersonalityInference => {
                RamariaError::llm(message)
            }
            JobType::IndexRebuild => RamariaError::index(message),
            JobType::Calibration | JobType::Custom(_) => RamariaError::validation(message),
        }
    }

    // =========================================================
    // 生命周期管理
    // =========================================================

    /// 创建一个新的后台任务（初始状态为 pending）。
    ///
    /// 参数:
    /// - `job_type`: 任务类型。
    /// - `payload`: 可选的任务参数（JSON 字符串）。
    ///
    /// 返回:
    /// - 新创建任务的数据库 id。
    pub async fn create(&self, job_type: JobType, payload: Option<&str>) -> RamariaResult<i64> {
        let id = self
            .storage
            .create_background_job(job_type.as_str(), payload)
            .await?;
        info!(
            job_id = id,
            job_type = %job_type,
            "后台任务已创建"
        );
        Ok(id)
    }

    /// 将任务状态标记为 running。
    pub async fn mark_running(&self, job_id: i64) -> RamariaResult<()> {
        self.storage
            .update_job_status(job_id, status::RUNNING, None)
            .await?;
        debug!(job_id = job_id, "后台任务开始执行");
        Ok(())
    }

    /// 将任务状态标记为 completed。
    pub async fn mark_completed(&self, job_id: i64) -> RamariaResult<()> {
        self.storage
            .update_job_status(job_id, status::COMPLETED, None)
            .await?;
        info!(job_id = job_id, "后台任务执行成功");
        Ok(())
    }

    /// 将任务状态标记为 failed（含错误信息）。
    pub async fn mark_failed(&self, job_id: i64, error_msg: &str) -> RamariaResult<()> {
        self.storage
            .update_job_status(job_id, status::FAILED, Some(error_msg))
            .await?;
        error!(job_id = job_id, error = %error_msg, "后台任务执行失败");
        Ok(())
    }

    /// 将任务状态标记为 fatal（存储不可用等致命错误，无法继续）。
    ///
    /// 与 `mark_failed` 的区别:
    /// - `failed`: 任务逻辑执行失败（如 LLM 超时），可重试或告警。
    /// - `fatal`: 状态标记本身失败（如 storage 不可用），任务无法正常追踪，立即终止。
    pub async fn mark_fatal(&self, job_id: i64, error_msg: &str) -> RamariaResult<()> {
        self.storage
            .update_job_status(job_id, status::FATAL, Some(error_msg))
            .await?;
        error!(job_id = job_id, error = %error_msg, "后台任务遇到致命错误，已终止");
        Ok(())
    }

    /// 将任务状态标记为 retrying（含错误信息）。
    async fn mark_retrying(&self, job_id: i64, error_msg: &str) -> RamariaResult<()> {
        self.storage
            .update_job_status(job_id, status::RETRYING, Some(error_msg))
            .await?;
        warn!(
            job_id = job_id,
            error = %error_msg,
            "后台任务失败，将重试"
        );
        Ok(())
    }

    // =========================================================
    // 重试逻辑
    // =========================================================

    /// 带自动重试的任务执行包装器（支持 CancellationToken）。
    ///
    /// 流程:
    /// 1. 创建任务
    /// 2. 标记 running（失败 → 标记 fatal 并终止）
    /// 3. 检查取消令牌
    /// 4. 执行闭包 f
    /// 5. 若成功 → 标记 completed
    /// 6. 若失败 → 按 (Retryable/Fatal) 分类：
    /// - Retryable: 标记 retrying（失败 → fatal 终止）→ 检查取消令牌 → 指数退避等待 → 检查取消令牌 → 标记 running（失败 → fatal 终止）→ 重试
    /// - Fatal: 立即标记 failed
    ///
    /// 参数:
    /// - `job_type`: 任务类型。
    /// - `payload`: 可选的任务参数。
    /// - `cancel_token`: 可选取消令牌，`is_cancelled` 时优雅退出。
    /// - `f`: 异步闭包，返回 `JobResult`。
    ///
    /// 返回:
    /// - 成功时返回 job_id。
    /// - 取消或致命错误时返回 `RamariaError`。
    pub async fn execute_with_retry<F, Fut>(
        &self,
        job_type: JobType,
        payload: Option<&str>,
        cancel_token: Option<CancellationToken>,
        f: F,
    ) -> RamariaResult<i64>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = JobResult>,
    {
        let job_id = self.create(job_type, payload).await?;

        // 标记 running，失败时标记 fatal 并终止
        if let Err(e) = self.mark_running(job_id).await {
            let msg = format!("标记 running 失败 (storage 不可用): {}", e);
            let _ = self.mark_fatal(job_id, &msg).await;
            return Err(self.error_for_job(job_type, msg));
        }

        let mut attempts = 0u32;

        loop {
            // 检查取消令牌（每次循环开始前检查）
            if let Some(ref token) = cancel_token
                && token.is_cancelled()
            {
                let msg = "任务已被取消（应用正在关闭）".to_string();
                self.mark_failed(job_id, &msg).await?;
                return Err(self.error_for_job(job_type, msg));
            }

            attempts += 1;
            debug!(job_id = job_id, attempt = attempts, "执行任务尝试");

            match f().await {
                JobResult::Success => {
                    self.mark_completed(job_id).await?;
                    return Ok(job_id);
                }
                JobResult::Fatal(err) => {
                    self.mark_failed(job_id, &err).await?;
                    return Err(self.error_for_job(
                        job_type,
                        format!("任务 {} (id={}) 致命错误: {}", job_type, job_id, err),
                    ));
                }
                JobResult::Retryable(err) => {
                    if attempts >= self.config.max_retries {
                        self.mark_failed(job_id, &err).await?;
                        return Err(self.error_for_job(
                            job_type,
                            format!(
                                "任务 {} (id={}) 已达最大重试次数 {}: {}",
                                job_type, job_id, self.config.max_retries, err
                            ),
                        ));
                    }

                    // 标记 retrying，失败时标记 fatal 并终止
                    if let Err(e) = self.mark_retrying(job_id, &err).await {
                        let msg = format!(
                            "标记 retrying 失败 (storage 不可用): {} (原始错误: {})",
                            e, err
                        );
                        let _ = self.mark_fatal(job_id, &msg).await;
                        return Err(self.error_for_job(job_type, msg));
                    }

                    // 指数退避: delay = base * 2^(attempt-1)
                    let delay_ms =
                        self.config.retry_base_delay_ms * 2u64.pow(attempts.saturating_sub(1));
                    let delay_ms = delay_ms.min(60_000); // 上限 60 秒

                    warn!(
                        job_id = job_id,
                        attempt = attempts,
                        delay_ms = delay_ms,
                        "指数退避等待后重试"
                    );

                    // 检查取消令牌（sleep 前）
                    if let Some(ref token) = cancel_token
                        && token.is_cancelled()
                    {
                        let msg = "任务在重试等待前被取消（应用正在关闭）".to_string();
                        self.mark_failed(job_id, &msg).await?;
                        return Err(self.error_for_job(job_type, msg));
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                    // 检查取消令牌（sleep 后，标记 running 前）
                    if let Some(ref token) = cancel_token
                        && token.is_cancelled()
                    {
                        let msg = "任务在重试等待后被取消（应用正在关闭）".to_string();
                        self.mark_failed(job_id, &msg).await?;
                        return Err(self.error_for_job(job_type, msg));
                    }

                    // 标记 running，失败时标记 fatal 并终止
                    if let Err(e) = self.mark_running(job_id).await {
                        let msg = format!(
                            "标记 running 失败 (storage 不可用，重试 #{}/{}): {}",
                            attempts, self.config.max_retries, e
                        );
                        let _ = self.mark_fatal(job_id, &msg).await;
                        return Err(self.error_for_job(job_type, msg));
                    }
                }
            }
        }
    }

    // =========================================================
    // 查询
    // =========================================================

    /// 列出所有 pending 状态的任务。
    ///
    /// 返回:
    /// - `Vec<(job_id, job_type, payload)>` 元组列表。
    pub async fn list_pending(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        let jobs = self.storage.list_pending_jobs().await?;
        debug!(count = jobs.len(), "查询到 pending 任务");
        Ok(jobs)
    }

    /// 获取任务管理器配置的引用。
    pub fn config(&self) -> &JobManagerConfig {
        &self.config
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_type_as_str() {
        assert_eq!(JobType::L1Summary.as_str(), "l1_summary");
        assert_eq!(JobType::EventExtract.as_str(), "event_extract");
        assert_eq!(
            JobType::PersonalityInference.as_str(),
            "personality_inference"
        );
        assert_eq!(JobType::Calibration.as_str(), "calibration");
        assert_eq!(JobType::IndexRebuild.as_str(), "index_rebuild");
        assert_eq!(JobType::Custom("custom_task").as_str(), "custom_task");
    }

    #[test]
    fn test_job_type_display() {
        assert_eq!(format!("{}", JobType::L1Summary), "l1_summary");
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

    #[test]
    fn test_job_manager_config_default() {
        let cfg = JobManagerConfig::default();
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.retry_base_delay_ms, 1000);
    }

    #[test]
    fn test_job_manager_config_custom() {
        let cfg = JobManagerConfig {
            max_retries: 5,
            retry_base_delay_ms: 2000,
        };
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.retry_base_delay_ms, 2000);
    }

    #[test]
    fn test_job_result_variants() {
        // 验证枚举变体可构造
        let _ = JobResult::Success;
        let _ = JobResult::Retryable("network timeout".into());
        let _ = JobResult::Fatal("disk full".into());
    }

    /// 验证 CancellationToken::cancel 后 is_cancelled 返回 true。
    #[test]
    fn test_cancellation_token_basic() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    /// 验证 CancellationToken clone 共享同一取消状态。
    #[test]
    fn test_cancellation_token_clone_shares_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
    }
}
