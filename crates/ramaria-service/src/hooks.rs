//! crates/ramaria-service/src/hooks.rs - 封存钩子默认装配
//!
//! 设计特点:
//! - 两套默认链路：轻量链（单 persona 最小 L2 触发）与完整链（全 persona L2 提取、
//!   知识事实抽取与 L3 级联），供宿主按响应语义选择注册
//! - 钩子不持有 Engine：装配阶段不捕获依赖快照，依赖在调用时经传入的引擎引用读取，
//!   避免长驻进程 Engine ↔ 钩子引用环导致内存不回收
//! - 编排复用：各项步骤走 `ramaria-memory` 的共用编排与提取器，不在本层重写算法
//! - 钩子内部自行处理失败并记 warn（满足钩子契约：不 panic、不长时间阻塞封存）
//! - 开关门控：`[behavior].enabled` / `[style].enabled` 关闭时对应步骤直接跳过
//! - 隐私：日志只记 persona 与错误摘要，不记规则文本、原文或事件内容

use std::pin::Pin;
use std::sync::Arc;

use crate::engine::Engine;
use crate::seal::{SealHook, SealHooks};

// =========================================================
// 默认装配
// =========================================================

/// 装配轻量封存钩子链（行为 / 风格 / 单 persona 最小 L2 触发）。
///
/// 用法:
/// - 以快速响应为语义的宿主（MCP 服务端等）在引擎装配后调用：
///   `engine.set_seal_hooks(default_seal_hooks(&engine))`；
/// - L2 触发只检查封存会话的 persona，不级联 L3、不做知识事实抽取。
///
/// 参数:
/// - `engine`: 服务层引擎（装配时读取配置开关供日志记录；钩子本体不持有引擎）。
///
/// 返回:
/// - 三个步骤均已注册的钩子集合；各步骤按配置开关与数据条件自行跳过。
pub fn default_seal_hooks(engine: &Engine) -> SealHooks {
    let hooks = SealHooks {
        behavior: Some(behavior_hook()),
        style: Some(style_hook()),
        l2_trigger: Some(l2_hook()),
    };
    log_assembled_chain(engine, &hooks, "轻量");
    hooks
}

/// 装配完整封存钩子链（行为 / 风格 / 全 persona L2 提取 + 知识事实抽取 + L3 级联）。
///
/// 用法:
/// - 具备完整离线学习预期的长驻宿主（桌面 / CLI 等）在引擎装配后调用：
///   `engine.set_seal_hooks(full_seal_hooks(&engine))`；
/// - L2 提取成功后按 `[knowledge].auto_fact_detect` 执行知识事实抽取，并级联 L3 推断。
///
/// 参数:
/// - `engine`: 服务层引擎（装配时读取配置开关供日志记录；钩子本体不持有引擎）。
///
/// 返回:
/// - 三个步骤均已注册的钩子集合；各步骤按配置开关与数据条件自行跳过。
pub fn full_seal_hooks(engine: &Engine) -> SealHooks {
    let hooks = SealHooks {
        behavior: Some(behavior_hook()),
        style: Some(style_hook()),
        l2_trigger: Some(full_l2_hook()),
    };
    log_assembled_chain(engine, &hooks, "完整");
    hooks
}

/// 记录钩子链装配结果（注册步骤 + 配置门控 + 链路形态）。
///
/// 隐私:
/// - 只记布尔与链路形态，不记规则文本、原文或事件内容。
fn log_assembled_chain(engine: &Engine, hooks: &SealHooks, chain: &str) {
    let config = engine.config();
    tracing::debug!(
        chain,
        behavior = hooks.behavior.is_some(),
        style = hooks.style.is_some(),
        l2_trigger = hooks.l2_trigger.is_some(),
        behavior_enabled = config.behavior.enabled,
        style_enabled = config.style.enabled,
        "封存钩子链已装配"
    );
}

// =========================================================
// 钩子实现（依赖在调用时从传入引擎读取，不持有引擎）
// =========================================================

/// 行为规则增量更新钩子（`[behavior].enabled` 关闭时跳过）。
fn behavior_hook() -> SealHook {
    Arc::new(
        |engine: &Engine, persona_uid: &str| -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let persona = persona_uid.to_string();
            Box::pin(async move {
                let config = engine.config();
                if !config.behavior.enabled {
                    tracing::debug!(persona_uid = %persona, "行为层已关闭，跳过行为规则增量更新");
                    return;
                }
                let llm = engine.llm_ref();
                let embedding = engine.embedding_ref();
                let pending = Arc::clone(engine.behavior_pending_ref());
                let result = ramaria_memory::behavior::orchestrate::incremental_update(
                    engine.storage_ref().as_ref(),
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
            })
        },
    )
}

/// 风格统计增量更新钩子（`[style].enabled` 关闭时跳过）。
fn style_hook() -> SealHook {
    Arc::new(
        |engine: &Engine, persona_uid: &str| -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let persona = persona_uid.to_string();
            Box::pin(async move {
                let config = engine.config();
                if !config.style.enabled {
                    tracing::debug!(persona_uid = %persona, "表达层风格统计已关闭，跳过增量更新");
                    return;
                }
                let llm = engine.llm_ref();
                let result = ramaria_memory::style::orchestrate::incremental_update(
                    engine.storage_ref().as_ref(),
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
            })
        },
    )
}

/// 轻量链 L2 事件提取触发钩子（单 persona 最小触发，未达阈值时提取器入口自行跳过）。
///
/// 说明:
/// - 只检查传入 persona 的未吸收 L1，达到 `[thresholds].l2_trigger_count` 时执行一次提取；
/// - 不级联 L3、不做知识事实抽取；失败只记日志（钩子契约），不阻塞封存主流程。
fn l2_hook() -> SealHook {
    Arc::new(
        |engine: &Engine, persona_uid: &str| -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let persona = persona_uid.to_string();
            Box::pin(async move {
                let llm = engine.llm_ref();
                let result = crate::l2::check_and_extract(
                    engine.storage_ref().as_ref(),
                    llm.as_ref(),
                    &engine.config(),
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
            })
        },
    )
}

/// 完整链 L2 触发钩子（全 persona 检查 + 知识事实抽取 + L3 级联）。
///
/// 说明:
/// - 遍历全部 persona 检查未吸收 L1，达到阈值时执行事件提取
///   （经 `JobManager` 包裹，含重试与可观测性）；
/// - 提取成功后按 `[knowledge].auto_fact_detect` 执行知识事实抽取，并级联 L3 推断；
/// - 内部失败只记日志（与后台调度链同一实现），不阻塞封存主流程；
/// - 无宿主停止位（`None`），检查全程不中断。
fn full_l2_hook() -> SealHook {
    Arc::new(
        |engine: &Engine, _persona_uid: &str| -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move {
                crate::lifecycle::l2_l3::check_l2_trigger(engine, None).await;
            })
        },
    )
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, ScriptedLlm, engine_with_l1_reply, engine_with_shared_scripted_llm, seed_l1,
        seed_persona, seed_session_with_messages,
    };
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::ProfileField;

    /// L2 事件提取的规范回复（工作 / 加班主题；置信度与陈述方式满足知识抽取常规轨道）。
    const L2_EVENT_JSON: &str = r#"{"events": [{"title": "频繁加班", "summary": "用户最近工作压力很大，经常加班到深夜", "keywords": "工作,加班", "confidence": 0.9}]}"#;

    /// L3 推断 Step1 回复（分类信号 JSON 对象）。
    const L3_STEP1_JSON: &str = r#"{"工作": {"signal_label": "工作-持续投入", "evidence_citation": "n_eff=1.0", "stability_judgment": "stable", "sufficient_evidence": true}}"#;

    /// L3 推断 Step2 回复（跨分类一致性 JSON 对象）。
    const L3_STEP2_JSON: &str = r#"{"base_candidates": [], "primary_candidates": ["工作"], "accent_candidates": [], "notes": ""}"#;

    /// L3 推断 Step3 回复（结构化画像 JSON 数组）。
    const L3_STEP3_JSON: &str = r#"[{"layer": "primary", "trait_label": "工作-持续投入", "meaning": "对工作保持持续投入", "confidence": 0.72}]"#;

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

    /// 完整链端到端：封存 → L2 事件提取 → 知识事实抽取 → L3 级联推断。
    ///
    /// 脚本化 LLM 的调用序列:
    /// 1. L1 摘要（封存生成）；
    /// 2. L2 事件提取（单簇）；
    /// 3~5. L3 三步推断（Step1 / Step2 / Step3）。
    #[tokio::test]
    async fn full_chain_triggers_l2_and_cascades_l3_and_extracts_facts() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        config.thresholds.l3_trigger_count = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;
        config.knowledge.auto_fact_detect = true;

        let llm = Arc::new(ScriptedLlm::replies(&[
            L1_JSON_REPLY,
            L2_EVENT_JSON,
            L3_STEP1_JSON,
            L3_STEP2_JSON,
            L3_STEP3_JSON,
        ]));
        let (engine, storage, dir) =
            engine_with_shared_scripted_llm("hooks-full-chain", Arc::clone(&llm), config, None)
                .await;
        seed_persona(&storage, "char-0001").await;
        // 种子：与封存摘要关键词连通的两条未吸收 L1（保证单簇 → 单次提取调用）
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班到深夜",
            Some("工作压力,加班"),
            1_000,
        )
        .await;
        seed_l1(
            &storage,
            "char-0001",
            "用户提到项目上线前每天都在加班",
            Some("工作压力,加班"),
            2_000,
        )
        .await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        engine.set_seal_hooks(full_seal_hooks(&engine));
        let outcome = engine.seal(session_id).await.expect("封存应成功");
        assert!(outcome.sealed, "本次调用应抢到封存权");
        assert_eq!(outcome.l1_count, 1, "短会话应生成单条 L1");

        // ① 事件已落库（L2 提取完成；最终被 L3 吸收，见 ③）
        let events = storage
            .list_events_by_persona("char-0001", 0, 100)
            .await
            .expect("查询事件应成功");
        assert!(!events.is_empty(), "L2 提取应产出事件并落库");

        // ② 知识事实已落库（auto_fact_detect 增强层；工作 / 加班事件 → PersonalStatus）
        let facts = storage
            .list_facts_by_persona("char-0001", ProfileField::PersonalStatus)
            .await
            .expect("查询事实应成功");
        assert!(
            !facts.is_empty(),
            "auto_fact_detect 应从本批事件抽取事实并落库"
        );

        // ③ L3 级联已触达并完成：trait 落库、事件被吸收
        let traits = storage
            .list_traits_by_persona("char-0001")
            .await
            .expect("查询 trait 应成功");
        assert!(!traits.is_empty(), "L3 级联推断应产出 trait");
        assert!(
            storage
                .list_unabsorbed_events("char-0001")
                .await
                .expect("查询未吸收事件应成功")
                .is_empty(),
            "L3 完成后事件应被标记吸收"
        );

        // ④ LLM 调用序列：1 次 L1 摘要 + 1 次 L2 提取 + 3 次 L3 三步推断
        assert_eq!(
            llm.call_count(),
            5,
            "完整链应恰好发起 5 次 LLM 调用（L1 + L2 + L3 三步）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 轻量链行为：只对封存会话的 persona 触发 L2 提取，不级联 L3。
    ///
    /// 对照点:
    /// - 封存目标 persona：L1 被吸收、事件落库；
    /// - 旁证 persona（同样达标但未被封存）：未吸收 L1 保持原状（不触达）；
    /// - 无 L3 级联：事件保持未吸收，LLM 调用恰为 2 次（L1 摘要 + 单簇提取）。
    #[tokio::test]
    async fn default_chain_l2_hook_is_single_persona() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;

        let llm = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY, L2_EVENT_JSON]));
        let (engine, storage, dir) =
            engine_with_shared_scripted_llm("hooks-default-chain", Arc::clone(&llm), config, None)
                .await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        // 封存目标：两条关键词连通的未吸收 L1
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大",
            Some("工作压力,加班"),
            1_000,
        )
        .await;
        seed_l1(
            &storage,
            "char-0001",
            "用户说项目上线前天天加班",
            Some("工作压力,加班"),
            2_000,
        )
        .await;
        // 旁证 persona：同样达阈值的未吸收 L1（轻量链不应触达）
        seed_l1(
            &storage,
            "char-0002",
            "用户最近睡眠质量不好",
            Some("睡眠"),
            1_000,
        )
        .await;
        let session_id = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        engine.set_seal_hooks(default_seal_hooks(&engine));
        let outcome = engine.seal(session_id).await.expect("封存应成功");
        assert!(outcome.sealed);

        // 封存目标触发提取：其未吸收 L1 已吸收、事件落库
        assert!(
            storage
                .list_unabsorbed_l1("char-0001")
                .await
                .expect("查询未吸收 L1 应成功")
                .is_empty(),
            "轻量链应对封存会话的 persona 触发提取并吸收其 L1"
        );
        assert!(
            !storage
                .list_events_by_persona("char-0001", 0, 100)
                .await
                .expect("查询事件应成功")
                .is_empty(),
            "轻量链应产出事件"
        );

        // 旁证 persona 未被触达：未吸收 L1 保持原状
        assert_eq!(
            storage
                .list_unabsorbed_l1("char-0002")
                .await
                .expect("查询未吸收 L1 应成功")
                .len(),
            1,
            "轻量链只检查封存会话的 persona，不应触达其他 persona"
        );

        // 无 L3 级联：事件保持未吸收
        assert_eq!(
            storage
                .list_unabsorbed_events("char-0001")
                .await
                .expect("查询未吸收事件应成功")
                .len(),
            1,
            "轻量链不级联 L3，事件应保持未吸收"
        );

        // LLM 调用恰好 2 次：L1 摘要 + 单簇提取
        assert_eq!(llm.call_count(), 2, "轻量链不应发起 L3 推断等额外 LLM 调用");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
