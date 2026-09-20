//! crates/ramaria-service/src/hooks.rs - 默认封存钩子装配
//!
//! 设计特点:
//! - 供无 app 宿主的入口（MCP 服务端等）在启动时注册：行为规则 / 风格统计 / L2 触发
//! - 编排复用：三项步骤走 `ramaria-memory` 的共用编排与提取器，不在本层重写算法
//! - 依赖以 Arc 快照捕获（存储 / LLM / 嵌入 / 配置 / 待定池），**不持有 Engine**，
//!   避免 Engine ↔ 钩子引用环导致长驻进程内存不回收
//! - 钩子内部自行处理失败并记 warn（满足钩子契约：不 panic、不长时间阻塞封存）
//! - 开关门控：`[behavior].enabled` / `[style].enabled` 关闭时对应步骤直接跳过
//! - 隐私：日志只记 persona 与错误摘要，不记规则文本、原文或事件内容

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ramaria_core::traits::{EmbeddingProvider, LlmProvider};
use ramaria_memory::behavior::PendingPool;

use crate::engine::Engine;
use crate::seal::{SealHook, SealHooks};

/// 装配默认封存钩子（行为 / 风格 / L2 触发）。
///
/// 用法:
/// - 无 app 宿主的入口在引擎装配后调用：`engine.set_seal_hooks(default_seal_hooks(&engine))`；
/// - 桌面与 CLI 仍走 app 侧既有钩子（含 L2→L3 级联与知识事实抽取等完整链路）。
///
/// 参数:
/// - `engine`: 服务层引擎（仅用于取依赖快照，钩子不持有引擎本身）。
///
/// 返回:
/// - 三个步骤均已注册的钩子集合；各步骤按配置开关与数据条件自行跳过。
pub fn default_seal_hooks(engine: &Engine) -> SealHooks {
    SealHooks {
        behavior: Some(behavior_hook(engine)),
        style: Some(style_hook(engine)),
        l2_trigger: Some(l2_hook(engine)),
    }
}

/// 行为规则增量更新钩子（`[behavior].enabled` 关闭时跳过）。
fn behavior_hook(engine: &Engine) -> SealHook {
    let storage = Arc::clone(engine.storage_ref());
    let llm: Arc<dyn LlmProvider> = engine.llm();
    let embedding: Option<Arc<dyn EmbeddingProvider>> = engine.embedding();
    let config = Arc::new(engine.config().clone());
    let pending = Arc::clone(engine.behavior_pending_ref());

    Arc::new(move |persona_uid: &str| {
        let storage = Arc::clone(&storage);
        let llm = Arc::clone(&llm);
        let embedding = embedding.clone();
        let config = Arc::clone(&config);
        let pending: Arc<Mutex<PendingPool>> = Arc::clone(&pending);
        let persona = persona_uid.to_string();

        Box::pin(async move {
            if !config.behavior.enabled {
                tracing::debug!(persona_uid = %persona, "行为层已关闭，跳过行为规则增量更新");
                return;
            }
            let result = ramaria_memory::behavior::orchestrate::incremental_update(
                storage.as_ref(),
                llm.as_ref(),
                embedding.as_deref(),
                &config.behavior,
                pending.as_ref(),
                &persona,
            )
            .await;
            if let Err(e) = result {
                // 钩子契约：失败只记日志，不 panic、不把错误抛回封存主流程
                tracing::warn!(
                    persona_uid = %persona,
                    error = %e,
                    "行为规则增量更新失败（不阻塞封存）"
                );
            }
        }) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    })
}

/// 风格统计增量更新钩子（`[style].enabled` 关闭时跳过）。
fn style_hook(engine: &Engine) -> SealHook {
    let storage = Arc::clone(engine.storage_ref());
    let llm: Arc<dyn LlmProvider> = engine.llm();
    let config = Arc::new(engine.config().clone());

    Arc::new(move |persona_uid: &str| {
        let storage = Arc::clone(&storage);
        let llm = Arc::clone(&llm);
        let config = Arc::clone(&config);
        let persona = persona_uid.to_string();

        Box::pin(async move {
            if !config.style.enabled {
                tracing::debug!(persona_uid = %persona, "表达层风格统计已关闭，跳过增量更新");
                return;
            }
            let result = ramaria_memory::style::orchestrate::incremental_update(
                storage.as_ref(),
                Some(llm.as_ref()),
                &config.style,
                &persona,
            )
            .await;
            if let Err(e) = result {
                tracing::warn!(
                    persona_uid = %persona,
                    error = %e,
                    "风格统计增量更新失败（不阻塞封存）"
                );
            }
        }) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    })
}

/// L2 事件提取触发钩子（未达阈值时提取器入口自行跳过，失败只记日志）。
fn l2_hook(engine: &Engine) -> SealHook {
    let storage = Arc::clone(engine.storage_ref());
    let llm: Arc<dyn LlmProvider> = engine.llm();
    let config = Arc::new(engine.config().clone());

    Arc::new(move |persona_uid: &str| {
        let storage = Arc::clone(&storage);
        let llm = Arc::clone(&llm);
        let config = Arc::clone(&config);
        let persona = persona_uid.to_string();

        Box::pin(async move {
            let result = crate::l2::check_and_extract(
                storage.as_ref(),
                llm.as_ref(),
                config.as_ref(),
                &persona,
            )
            .await;
            if let Err(e) = result {
                // LLM 不可用 / 存储异常均降级：事件层下次封存再尝试，不影响记忆落库
                tracing::warn!(
                    persona_uid = %persona,
                    error = %e,
                    "L2 事件提取触发失败（不阻塞封存）"
                );
            }
        }) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, engine_with_l1_reply, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::traits::StoreCrud;

    /// 默认钩子注册后封存：L1 正常生成，风格统计步骤落库，行为 / L2 按数据条件跳过。
    #[tokio::test]
    async fn seal_with_default_hooks_updates_style_stats() {
        let (engine, storage, dir) = engine_with_l1_reply("hooks-seal", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 6, 1_000).await;

        engine.set_seal_hooks(default_seal_hooks(&engine));
        let outcome = engine.seal(session_id).await.expect("封存应成功");
        assert!(outcome.sealed, "本次调用应抢到封存权");
        assert_eq!(outcome.l1_count, 1, "应生成一条 L1 摘要");

        // 风格钩子已执行：persona_style_stats 出现记录（样本不足 → Insufficient 也算执行）
        let stats = storage
            .get_style_stats("char-0001")
            .await
            .expect("读取风格统计应成功");
        assert!(stats.is_some(), "默认钩子的风格统计步骤应已执行");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
