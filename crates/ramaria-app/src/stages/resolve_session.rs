//! crates/ramaria-app/src/stages/resolve_session.rs - Stage 3: 会话管理
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 3: 会话管理
//! - 无 session_id → 创建新 session
//! - 有 session_id → 验证存在 + 只读约束（已关闭的 session 拒绝写入）
//! - 同步追踪活跃 session 到 SessionLifecycle（供 save_and_close_session 查找）
//! - 记录 session 活跃时间（供空闲检测线程使用）

use async_trait::async_trait;
use ramaria_core::error::RamariaError;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

/// Stage 3: 会话管理（含只读约束 + persona_uid 记录）。
///
/// 职责:
/// - 根据 PipelineData.session_id 决定创建新会话还是复用已有会话
/// - 验证已有会话存在且未关闭（只读约束）
/// - 将解析后的 Session 写入 PipelineData.session
/// - 同步活跃 session ID 到 SessionLifecycle
/// - 记录 session 活跃时间到 SessionLifecycle 内存缓存
///
/// 分支逻辑:
/// - `session_id = Some(sid)`:
///   1. 查询 storage.get_session(sid)
///   2. 不存在 → Fatal Validation Error
///   3. ended_at IS NOT NULL → Fatal Validation Error（已关闭不可写入）
///   4. 设置活跃 session → touch_session
/// - `session_id = None`:
///   1. 调用 storage.create_session()
///   2. 设置活跃 session → touch_session
pub struct StageResolveSession;

impl StageResolveSession {
    /// 创建 StageResolveSession 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageResolveSession {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageResolveSession {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "ResolveSession"
    }

    /// 执行会话管理。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（读取 storage 和 lifecycle）。
    /// - `input`: 管线数据，读取 `session_id` 字段。
    ///
    /// 返回:
    /// - `Ok(data)`: 会话解析成功，`data.session` 已填充。
    /// - `Err(Fatal)`: 会话不存在或已关闭。
    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let session = match input.session_id {
            Some(sid) => {
                tracing::debug!(session_id = %sid, "使用前端传入的 session_id");

                let s = ctx
                    .storage
                    .get_session(sid)
                    .await
                    .map_err(|e| {
                        PipelineError::fatal(
                            "ResolveSession",
                            RamariaError::storage_with_source(
                                format!("查询 session {sid} 失败"),
                                e,
                            ),
                        )
                    })?
                    .ok_or_else(|| {
                        PipelineError::fatal(
                            "ResolveSession",
                            RamariaError::validation(format!("会话不存在: {sid}")),
                        )
                    })?;

                // 只读约束：已关闭的 session 不可发送消息
                if s.ended_at.is_some() {
                    tracing::warn!(session_id = %sid, "会话已关闭，拒绝写入");
                    return Err(PipelineError::fatal(
                        "ResolveSession",
                        RamariaError::validation(format!(
                            "会话已关闭（session {sid}），请开启新对话。"
                        )),
                    ));
                }

                // 存量 NULL 会话归属回写。
                // 会话创建时未绑定（persona_uid=NULL）且前端传入 uid 时，
                // 回写 DB 绑定，保证保存/封存时 L1、utt、examples 归属正确。
                // 回写失败仅 warn 不阻塞发送（绑定是增强，非消息发送前置条件）。
                // 注意：多客户端并发下为"最后写者胜"语义——单用户桌面场景
                // 窗口极小，可接受；如未来支持多端并发可在此加乐观锁。
                let mut s = s;
                if s.persona_uid.is_none()
                    && let Some(uid) = input.persona_uid.clone()
                {
                    match ctx.storage.bind_session_persona_uid(s.id, &uid).await {
                        Ok(()) => {
                            s.persona_uid = Some(uid.clone());
                            tracing::info!(
                                session_id = %s.id,
                                persona_uid = %uid,
                                "会话 NULL 归属已回写绑定"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %s.id,
                                %e,
                                "会话 NULL 归属回写失败（不阻塞消息发送）"
                            );
                        }
                    }
                }

                // 同步追踪活跃 session（否则 save_and_close_session 找不到）
                ctx.lifecycle.set_active_session_id_public(Some(s.id));

                s
            }
            None => {
                tracing::debug!(
                    persona_uid = input.persona_uid.as_deref().unwrap_or("none"),
                    "无 session_id，创建新 session"
                );

                // 创建 session 时绑定当前 persona_uid
                let s = ctx
                    .storage
                    .create_session(input.persona_uid.as_deref())
                    .await
                    .map_err(|e| {
                        PipelineError::fatal(
                            "ResolveSession",
                            RamariaError::storage_with_source("创建 session 失败", e),
                        )
                    })?;

                ctx.lifecycle.set_active_session_id_public(Some(s.id));
                tracing::info!(session_id = %s.id, "自动创建新 session");

                // v1.4 M5：桥接加载——新会话创建时取
                // 最近一个已关闭会话的尾部原文（utt 块优先，降级末 N 条原文）。
                // 开关关闭 / 注入闸门关闭（探针消融 F4/B0/B1/S_*）/
                // 白名单外 / 无上一会话 → 不注入（等同 v1.3），不阻塞。
                if ctx.config.injection.bridge {
                    let bridge = crate::bridge::load_bridge_context(
                        ctx.storage.as_ref(),
                        &ctx.config.bridge,
                        &ctx.config.utt,
                        input.persona_uid.as_deref(),
                    )
                    .await;
                    if let Some(content) = bridge.content {
                        tracing::debug!(
                            session_id = %s.id,
                            source = ?bridge.source,
                            chars = content.chars().count(),
                            "新会话已加载桥接内容"
                        );
                        input.bridge_context = Some(content);
                    }
                } else {
                    tracing::debug!("桥接注入闸门关闭（探针消融），跳过桥接加载");
                }

                s
            }
        };

        // Session-Persona 绑定——优先使用 session 中的 persona_uid
        // 若 session 有 persona_uid（DB 中已绑定），覆盖前端传参
        // 若 session 无 persona_uid（存量数据），保持前端传参不变
        if session.persona_uid.is_some() {
            input.persona_uid = session.persona_uid.clone();
            tracing::debug!(
                persona_uid = input.persona_uid.as_deref(),
                "从 session 读取 persona_uid"
            );
        }

        // 记录 session 活跃时间（供空闲检测线程使用）
        ctx.lifecycle.touch_session(session.id);

        input.session = Some(session);
        Ok(input)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockLlm, MockStorage, test_context};
    use ramaria_core::traits::StorageBackend;
    use ramaria_core::types::AppState;
    use std::sync::Arc;
    use uuid::Uuid;

    fn make_data(session_id: Option<Uuid>) -> PipelineData {
        PipelineData::new("test".into(), None, session_id, Uuid::new_v4())
            .with_app_state(AppState::Ready)
    }

    #[tokio::test]
    async fn no_session_id_creates_new_session() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageResolveSession::new();
        let data = make_data(None);

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("should create session");
        let session = output.session.expect("session should be set");
        assert!(session.ended_at.is_none());
    }

    #[tokio::test]
    async fn valid_session_id_reuses_session() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_active_session(session_id);

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        let data = make_data(Some(session_id));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("should reuse session");
        let session = output.session.expect("session should be set");
        assert_eq!(session.id, session_id);
        assert!(session.ended_at.is_none());
    }

    #[tokio::test]
    async fn closed_session_rejected() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_closed_session(session_id);

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        let data = make_data(Some(session_id));

        let result = stage.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("closed session should be rejected"),
            Err(e) => e,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "ResolveSession");
        assert!(err.source_error().context().contains("已关闭"));
    }

    #[tokio::test]
    async fn non_existent_session_rejected() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageResolveSession::new();
        let data = make_data(Some(Uuid::new_v4()));

        let result = stage.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("non-existent session should be rejected"),
            Err(e) => e,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "ResolveSession");
        assert!(err.source_error().context().contains("不存在"));
    }

    #[tokio::test]
    async fn active_session_id_tracked_in_lifecycle() {
        let storage = Arc::new(MockStorage::new());
        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        let data = make_data(None);

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        let session = output.session.expect("session should be set");

        // lifecycle 应该追踪了活跃 session
        let active = ctx.lifecycle.get_active_session_id();
        assert_eq!(active, Some(session.id));
    }

    #[tokio::test]
    async fn stage_name_is_correct() {
        let stage = StageResolveSession::new();
        assert_eq!(stage.name(), "ResolveSession");
    }

    // 存量 NULL 会话在发送消息时回写绑定 persona_uid
    #[tokio::test]
    async fn null_persona_session_bound_from_input() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_active_session(session_id);

        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        // 前端传入 persona_uid，session 本身为 NULL（存量缺陷场景）
        let data = PipelineData::new(
            "hi".into(),
            Some("char-0001".into()),
            Some(session_id),
            Uuid::new_v4(),
        )
        .with_app_state(AppState::Ready);

        let output = stage.execute(&ctx, data).await.expect("should succeed");

        // 内存中 session 已绑定
        assert_eq!(
            output.session.as_ref().unwrap().persona_uid.as_deref(),
            Some("char-0001")
        );
        // DB 已回写持久化
        let stored = storage.get_session(session_id).await.unwrap().unwrap();
        assert_eq!(stored.persona_uid.as_deref(), Some("char-0001"));
    }

    // session 已绑定 persona_uid（DB 真相源）时不被前端覆盖，
    // 也不触发回写（幂等绑定）
    #[tokio::test]
    async fn bound_persona_session_keeps_db_truth() {
        let storage = Arc::new(MockStorage::new());
        let session = storage.create_session(Some("char-0002")).await.unwrap();
        let session_id = session.id;

        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        let data = PipelineData::new(
            "hi".into(),
            Some("char-0001".into()),
            Some(session_id),
            Uuid::new_v4(),
        )
        .with_app_state(AppState::Ready);

        let output = stage.execute(&ctx, data).await.expect("should succeed");
        assert_eq!(
            output.session.as_ref().unwrap().persona_uid.as_deref(),
            Some("char-0002"),
            "DB 已绑定 persona 优先，前端传参不覆盖"
        );
    }

    // 回写失败（存储不支持）时降级 warn，不阻塞消息发送，
    // 前端传入 uid 保留用于消息级归属
    #[tokio::test]
    async fn null_persona_bind_failure_does_not_block_send() {
        let storage = Arc::new(MockStorage::new());
        let session_id = Uuid::new_v4();
        storage.add_active_session(session_id);
        storage.set_bind_fails(true);

        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        let stage = StageResolveSession::new();
        let data = PipelineData::new(
            "hi".into(),
            Some("char-0001".into()),
            Some(session_id),
            Uuid::new_v4(),
        )
        .with_app_state(AppState::Ready);

        let output = stage
            .execute(&ctx, data)
            .await
            .expect("回写失败不应阻塞消息发送");

        // 内存态保持 NULL（回写失败），前端 uid 保留在 input 供消息归属
        assert_eq!(output.session.as_ref().unwrap().persona_uid, None);
        assert_eq!(
            output.persona_uid.as_deref(),
            Some("char-0001"),
            "前端传入 uid 保留"
        );
        let stored = storage.get_session(session_id).await.unwrap().unwrap();
        assert_eq!(stored.persona_uid, None, "DB 未绑定");
    }
}
