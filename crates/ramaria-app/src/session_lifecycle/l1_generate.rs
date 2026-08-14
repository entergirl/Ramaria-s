//! rust/crates/ramaria-app/src/session_lifecycle/l1_generate.rs - L1 摘要生成与重试
//!
//! 设计特点:
//! - 实现 `SessionLifecycle` 的 L1 摘要生成、手动重试、批量无级联生成
//! - `generate_l1_summary` 通过 JobManager 包裹执行，含指数退避重试
//! - `index_l1_into_retriever` 在 L1 生成后增量更新内存检索索引
//! - `regenerate_l1_no_cascade` 支持幂等性检查：已有目标 persona_uid 的 L1 则跳过
//! - `close_session_safe` 安全关闭 session（防已关闭重复操作）
//! - `summarize_with_summarizer` 作为 JobManager 闭包的适配器

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use ramaria_memory::l1::{L1Summarizer, L1SummarizerConfig};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::SessionLifecycle;

// =========================================================
// L1 摘要手动重试（公开 API）
// =========================================================

impl SessionLifecycle {
    /// 为指定 session 重新生成 L1 摘要（手动重试）。
    ///
    /// 职责:
    /// - 供 save_and_close_session 中 L1 失败后的手动补救。
    /// - session 可以已关闭，也可以仍在活跃中（shutdown 场景）。
    ///
    /// 参数:
    /// - `session_id`: 目标 session。
    /// - `persona_uid`: 人格标识。
    /// - `user_prefix`: 覆盖默认"用户："前缀。`None` 使用默认。
    /// - `assistant_prefix`: 覆盖默认"助手："前缀。`None` 使用默认。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 生成成功。
    /// - `Ok(None)`: session 无消息。
    pub async fn regenerate_l1(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        // 检查是否有消息可摘要
        let messages = storage.list_messages(session_id).await?;
        if messages.is_empty() {
            warn!(%session_id, "regenerate_l1: session 无消息，跳过");
            return Ok(None);
        }

        info!(%session_id, ?persona_uid, msg_count = messages.len(), "手动重试 L1 摘要");

        match self
            .generate_l1_summary(
                storage,
                llm,
                session_id,
                persona_uid,
                user_prefix,
                assistant_prefix,
            )
            .await
        {
            Ok(l1) => {
                info!(%session_id, l1_id = %l1.id, "L1 重试成功");
                // 增量更新 Retriever 索引
                self.index_l1_into_retriever(&l1);
                // 触发 L2 检查（路径 A）
                self.check_l2_trigger(storage, llm).await;
                Ok(Some(l1))
            }
            Err(e) => {
                error!(%session_id, %e, "L1 重试失败");
                Err(e)
            }
        }
    }

    /// 生成 L1 摘要但不触发 L2 级联（用于批量导入场景，全部 L1 完成后统一触发）。
    ///
    /// 幂等性：
    /// - 若 session 已有目标 persona_uid 的 L1 摘要 → 跳过生成（避免重复 LLM 调用）。
    /// - 若仅有 NULL-persona_uid 的旧摘要 → 删除后重新生成。
    /// - 若无任何摘要 → 直接生成。
    ///
    /// 说明:
    /// - 与 `regenerate_l1` 功能相同，但跳过末尾的 `check_l2_trigger` 调用。
    /// - 调用方应在全部 L1 生成完成后手动调用 `check_l2_trigger`。
    pub async fn regenerate_l1_no_cascade(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        let messages = storage.list_messages(session_id).await?;
        if messages.is_empty() {
            warn!(%session_id, "regenerate_l1_no_cascade: session 无消息，跳过");
            return Ok(None);
        }

        // 检查是否已存在目标 persona_uid 的 L1 摘要（幂等——避免重复 LLM 调用）
        if let Some(target_uid) = persona_uid {
            let existing = storage.list_memory_l1(session_id).await?;
            let already_has = existing
                .iter()
                .any(|l1| l1.persona_uid.as_deref() == Some(target_uid));
            if already_has {
                info!(
                    %session_id,
                    persona_uid = %target_uid,
                    "该 session 已有目标 persona 的 L1 摘要，跳过重新生成"
                );
                // 增量索引仍要确保（万一之前的 rebuild 跳过了）
                if let Some(l1) = existing
                    .into_iter()
                    .find(|l| l.persona_uid.as_deref() == Some(target_uid))
                {
                    self.index_l1_into_retriever(&l1);
                }
                return Ok(None);
            }
        }

        // 删除旧 NULL-persona_uid L1 摘要，再做生成
        let deleted = storage.delete_memory_l1_by_session(session_id).await?;
        if deleted > 0 {
            info!(%session_id, deleted, "已清理旧 NULL-persona_uid L1 摘要");
        }

        info!(%session_id, ?persona_uid, msg_count = messages.len(), "批量 L1 摘要（无级联）");

        match self
            .generate_l1_summary(
                storage,
                llm,
                session_id,
                persona_uid,
                user_prefix,
                assistant_prefix,
            )
            .await
        {
            Ok(l1) => {
                info!(%session_id, l1_id = %l1.id, "L1 生成成功（无级联）");
                // 增量更新 Retriever 索引（批量导入场景每批次一个 session）
                self.index_l1_into_retriever(&l1);
                Ok(Some(l1))
            }
            Err(e) => {
                error!(%session_id, %e, "L1 生成失败");
                Err(e)
            }
        }
    }
}

// =========================================================
// L1 摘要生成（内部辅助）
// =========================================================

impl SessionLifecycle {
    /// 为指定 session 生成 L1 摘要。
    ///
    /// 参数:
    /// - `persona_uid`: 当前对话人格的 UID，用于 L1 归属。
    /// - `user_prefix`: 覆盖默认"用户："前缀。`None` 使用默认，
    ///   `Some("")` 表示不添加前缀（消息内容已含发送者名称）。
    /// - `assistant_prefix`: 覆盖默认"助手："前缀。同上。
    ///
    /// max_tokens 策略（v1.4 截断修复）:
    /// - 优先从 `backend_config` 传播 `max_tokens`（与 chat 管线输出预算一致），
    ///   下限钳制到 `L1SummarizerConfig` 默认值（1024），防止用户将 chat
    ///   `max_tokens` 配得过小时破坏 L1 完整 JSON 输出（含 evidence_notes）。
    /// - backend_config 缺失/读取失败 → 使用 L1 默认值 1024。
    ///
    /// 对齐 Python `summarizer.summarize_session(session_id)`。
    pub(super) async fn generate_l1_summary(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<ramaria_core::types::MemoryL1> {
        let mut summarizer_config = L1SummarizerConfig::default();
        // 设置 persona_uid，确保 L1 摘要可被记忆页面按人格过滤查询到
        if let Some(uid) = persona_uid {
            summarizer_config.persona_uid = Some(uid.to_string());
        }
        // 导入场景覆盖对话前缀，避免"用户/助手"称呼污染摘要
        if let Some(prefix) = user_prefix {
            summarizer_config.user_prefix = prefix.to_string();
        }
        if let Some(prefix) = assistant_prefix {
            summarizer_config.assistant_prefix = prefix.to_string();
        }
        // L1 输出预算从 backend_config 传播（v1.4 截断修复）：
        // evidence_notes 结构化 JSON 输出需要更大预算，默认 512 易被截断；
        // 下限钳制到 L1 默认值，避免 chat max_tokens 小配置破坏 L1 完整性。
        if let Ok(Some(backend)) = storage.get_backend_config().await {
            let floor = summarizer_config.max_tokens;
            summarizer_config.max_tokens = backend.max_tokens.max(floor);
            debug!(
                max_tokens = summarizer_config.max_tokens,
                backend_max_tokens = backend.max_tokens,
                "L1 摘要 max_tokens 已从 backend_config 传播"
            );
        }
        let summarizer = L1Summarizer::new(llm, storage, summarizer_config);

        // 通过 JobManager 包装执行（带重试）
        let job_manager = JobManager::with_defaults(storage);
        let payload = serde_json::json!({ "session_id": session_id.to_string() }).to_string();

        let result = job_manager
            .execute_with_retry(JobType::L1Summary, Some(&payload), None, || {
                summarize_with_summarizer(&summarizer, session_id)
            })
            .await;

        match result {
            Ok(_job_id) => {
                // 摘要已写入存储，需要读取返回（JobManager 不返回业务结果）
                // 从 storage 读取刚生成的 L1
                let l1_list = storage.list_memory_l1(session_id).await?;
                l1_list
                    .into_iter()
                    .last()
                    .ok_or_else(|| RamariaError::validation("L1 摘要生成后无法读取"))
            }
            Err(e) => Err(e),
        }
    }

    /// 将 L1 摘要增量添加到 Retriever 内存索引。
    ///
    /// 职责:
    /// - 在 L1 摘要生成成功后立即调用，使新 L1 文档无需等待手动 `rebuild_retriever`
    ///   即可被 Stage 5 RAG 检索命中（D-V12-013）。
    ///
    /// 容错:
    /// - Retriever 未注入（向后兼容）→ 静默跳过。
    /// - Mutex 锁污染 → warn 日志 + 跳过。
    /// - 索引添加失败 → warn 日志 + 不阻塞 L2 级联检查。
    ///
    /// 参数:
    /// - `l1`: 刚生成的 L1 摘要记录。
    pub(super) fn index_l1_into_retriever(&self, l1: &ramaria_core::types::MemoryL1) {
        let ret_guard = match self.retriever.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("retriever lock poisoned during index_l1_into_retriever: {e}");
                return;
            }
        };
        if let Some(ref retriever_arc) = *ret_guard {
            // RwLock write() 用于索引写入（index_l1_record 需要 &mut self）
            match retriever_arc.write() {
                Ok(mut retriever) => {
                    if let Err(e) = retriever.index_l1_record(l1) {
                        warn!(
                            l1_id = %l1.id,
                            error = %e,
                            "增量更新 Retriever 索引失败（不影响 L2 级联）"
                        );
                    } else {
                        info!(
                            l1_id = %l1.id,
                            persona_uid = ?l1.persona_uid,
                            "L1 摘要已增量加入 Retriever 内存索引，即时可检索"
                        );
                    }
                }
                Err(e) => {
                    warn!("retriever 内部 Mutex poisoned: {e}");
                }
            }
        }
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 安全关闭 session（忽略已关闭的 session）。
///
/// 对齐 Python `database.close_session(sid)`。
pub(super) async fn close_session_safe(
    storage: &dyn StorageBackend,
    session_id: Uuid,
) -> RamariaResult<()> {
    // 先检查 session 是否已关闭
    let session = storage.get_session(session_id).await?;
    match session {
        Some(s) if s.ended_at.is_none() => {
            storage.close_session(session_id).await?;
            info!(%session_id, "session 已关闭");
            Ok(())
        }
        Some(_) => {
            debug!(%session_id, "session 已关闭，跳过");
            Ok(())
        }
        None => {
            warn!(%session_id, "session 不存在，跳过关闭");
            Ok(())
        }
    }
}

/// L1 摘要生成的异步闭包（供 JobManager::execute_with_retry 使用）。
pub(super) async fn summarize_with_summarizer(
    summarizer: &L1Summarizer<'_>,
    session_id: Uuid,
) -> JobResult {
    match summarizer.summarize_session(session_id).await {
        Ok(_l1) => {
            info!(%session_id, "L1 摘要生成成功");
            JobResult::Success
        }
        Err(e) => {
            // LLM 调用失败是可重试的（网络波动、服务暂时不可用等）
            warn!(%session_id, %e, "L1 摘要生成失败，将重试");
            JobResult::Retryable(e.to_string())
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::config::RamariaConfig;
    use ramaria_memory::retriever::Retriever;
    use std::sync::{Arc, RwLock};

    #[test]
    fn index_l1_into_retriever_without_set_retriever_is_noop() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        // 未注入 retriever 时调用不应 panic
        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: "测试".to_string(),
            keywords: None,
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("test".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };
        // 不应 panic
        lifecycle.index_l1_into_retriever(&l1);
    }

    #[test]
    fn index_l1_into_retriever_adds_to_index() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let retriever = Arc::new(RwLock::new(Retriever::new()));
        lifecycle.set_retriever(Arc::clone(&retriever));

        let sid = Uuid::new_v4();
        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: sid,
            summary: "测试摘要：用户讨论了Rust编程话题".to_string(),
            keywords: Some("Rust,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.5,
            salience: 0.8,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("rama-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        lifecycle.index_l1_into_retriever(&l1);

        // 验证 retriever 中已有文档（read() 即可，doc_count 为只读）
        let guard = retriever.read().unwrap();
        assert_eq!(guard.doc_count(), 1);
    }

    #[test]
    fn index_l1_into_retriever_makes_l1_searchable() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let retriever = Arc::new(RwLock::new(Retriever::new()));
        lifecycle.set_retriever(Arc::clone(&retriever));

        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
            keywords: Some("学习,Rust,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.8,
            salience: 0.9,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("rama-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
            continuation: None,
        };

        lifecycle.index_l1_into_retriever(&l1);

        // 立即检索，应能命中（read() 即可，search 为 &self）
        let guard = retriever.read().unwrap();
        let req = ramaria_memory::SearchRequest {
            query: "Rust".to_string(),
            persona_uid: None,
            top_k: 5,
            filter_share: false,
        };
        let results = guard.search(&req, None);
        assert!(!results.is_empty());
        assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
    }
}
