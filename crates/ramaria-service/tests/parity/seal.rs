//! crates/ramaria-service/tests/parity/seal.rs - 对照路径：封存（seal）
//!
//! 设计特点:
//! - fixture 固定：单 persona + 4 条消息的活跃会话（短会话 → 单条 L1，与用例测试同口径）
//! - 快照只含稳定字段：抢占结果 / L1 关键字段 / 会话结束状态 / 待定任务类型 / utt 与示例计数；
//!   不落 id、时间戳与 LLM 原始输出（易变值与隐私红线均不进入快照）
//! - 验证维度：基线一致（golden 冻结）、跨隔离环境确定性（`assert_stable`）、
//!   抢占幂等（二次封存不重复生成 L1）、手动关闭产物基线（完整钩子链下的端到端产物）
//! - 输出入口：`snapshot_of` 是"该 fixture 上的规范化输出"的唯一入口，
//!   快照直接送入 `GoldenStore` 冻结或比对（比较与报告实现与 `assert_stable` 共用）

use std::sync::Arc;

use ramaria_core::traits::{LlmProvider, StoreCrud, StoreInfrastructure};
use ramaria_core::types::CHANNEL_LOCAL;
use ramaria_service::types::ChatSendRequest;
use ramaria_storage::SqliteStorage;
use serde_json::json;
use uuid::Uuid;

use crate::support::env::DEFAULT_ASSISTANT_REPLY;
use crate::support::{
    GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot, assert_stable,
    fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "seal_single_session_l1";

/// fixture 人格 uid。
const PERSONA: &str = "char-parity-seal";

/// 固定 LLM 回复：符合 L1 摘要 JSON 契约（封存用例解析该结构落库）。
const L1_JSON_REPLY: &str = r#"{
  "summary": "用户最近工作压力很大，常加班到深夜。",
  "keywords": "工作压力,加班",
  "time_period": "夜间",
  "atmosphere": "疲惫",
  "valence": -0.4,
  "salience": 0.8,
  "situation_strength": 4
}"#;

/// 手动关闭产物场景名（同时作为 golden 基线文件名）。
const MANUAL_CLOSE_SCENARIO: &str = "seal_manual_close_products";

/// 手动关闭场景的用户消息（合成数据；发一轮消息以建立会话）。
const MANUAL_CLOSE_MESSAGE: &str = "今天有点累，随便聊聊吧";

// =========================================================
// 场景执行
// =========================================================

/// 造 fixture：persona 与带 4 条消息的活跃会话。
async fn fixture(env: &ParityEnv) -> ParityResult<Uuid> {
    fixtures::seed_persona(env.storage(), PERSONA).await?;
    fixtures::seed_active_session(env.storage(), PERSONA, 4, fixtures::fixture_ts(0)).await
}

/// 在给定环境上执行封存场景，产出规范化快照。
///
/// 流程:
/// 1. 造 fixture（persona + 活跃会话 + 消息）；
/// 2. 调用封存用例（抢占关闭 → L1 生成 → 索引镜像 → utt → 示例抽取）；
/// 3. 读取可观测结果（L1 / 会话状态 / 待定任务 / utt 与示例计数）并规范化。
async fn snapshot_of(env: &ParityEnv) -> ParityResult<Snapshot> {
    let session_id = fixture(env).await?;

    let outcome = env
        .engine()
        .seal(session_id)
        .await
        .map_err(|e| ParityError::env("执行封存用例", e))?;

    let l1_list = env
        .storage()
        .list_memory_l1(session_id)
        .await
        .map_err(|e| ParityError::env("读取封存后的 L1", e))?;
    let session = env
        .storage()
        .get_session(session_id)
        .await
        .map_err(|e| ParityError::env("读取封存后的会话", e))?
        .ok_or_else(|| ParityError::env("读取封存后的会话", "会话应存在"))?;
    let message_count = env
        .storage()
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("读取会话消息", e))?
        .len();
    let pending_jobs = env
        .storage()
        .list_pending_jobs()
        .await
        .map_err(|e| ParityError::env("读取待定后台任务", e))?;
    let utt_count = env
        .storage()
        .list_utt_blocks_by_persona(PERSONA)
        .await
        .map_err(|e| ParityError::env("读取 utt 话语块", e))?
        .len();
    let example_count = env
        .storage()
        .list_all_examples(PERSONA)
        .await
        .map_err(|e| ParityError::env("读取对话示例", e))?
        .len();

    let l1_snapshot: Vec<serde_json::Value> = l1_list
        .iter()
        .map(|l1| {
            json!({
                "summary": l1.summary.clone(),
                "keywords": l1.keywords.clone(),
                "time_period": l1.time_period.clone(),
                "atmosphere": l1.atmosphere.clone(),
                "valence": l1.valence,
                "salience": l1.salience,
                "situation_strength": l1.situation_strength,
                "persona_uid": l1.persona_uid.clone(),
            })
        })
        .collect();

    Ok(Snapshot::new(
        SCENARIO,
        json!({
            "outcome": {
                "sealed": outcome.sealed,
                "l1_count": outcome.l1_count,
            },
            "session": {
                "ended": session.ended_at.is_some(),
                "message_count": message_count,
            },
            "l1": l1_snapshot,
            "pending_job_types": pending_jobs
                .iter()
                .map(|(_, job_type, _)| job_type.clone())
                .collect::<Vec<_>>(),
            "utt_block_count": utt_count,
            "example_count": example_count,
        }),
    ))
}

/// 以固定 L1 回复的脚本 LLM 构造环境（每次调用消费一条脚本回复）。
async fn env_with_l1_reply(tag: &str) -> ParityResult<ParityEnv> {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY]));
    ParityEnv::with_llm(tag, llm).await
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：封存场景输出与冻结基线逐字段一致。
#[tokio::test]
async fn seal_snapshot_matches_golden_baseline() {
    let env = env_with_l1_reply("seal-golden")
        .await
        .expect("封存对照环境应可构建");
    let snapshot = snapshot_of(&env).await.expect("封存场景应执行成功");

    // 关键行为断言：抢占成功且生成单条 L1（快照之外的语义锚点）
    let sealed = snapshot.value()["outcome"]["sealed"]
        .as_bool()
        .expect("快照应含 sealed 布尔值");
    assert!(sealed, "首次封存应抢到关闭权");
    assert_eq!(
        snapshot.value()["outcome"]["l1_count"].as_u64(),
        Some(1),
        "短会话应生成单条 L1"
    );

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
    tracing::info!(
        path = %outcome.path().display(),
        ?outcome,
        "封存基线比对完成"
    );

    env.cleanup().await;
}

/// 确定性：两个隔离环境各自执行同一场景，输出应完全一致（无随机与时钟漂移）。
#[tokio::test]
async fn seal_snapshots_are_stable_across_isolated_envs() {
    let first_env = env_with_l1_reply("seal-stable-a")
        .await
        .expect("首个对照环境应可构建");
    let first = snapshot_of(&first_env).await.expect("首轮执行应成功");
    first_env.cleanup().await;

    let second_env = env_with_l1_reply("seal-stable-b")
        .await
        .expect("第二个对照环境应可构建");
    let replay = snapshot_of(&second_env).await.expect("第二轮执行应成功");
    second_env.cleanup().await;

    assert_stable("seal/single-session-l1", &first, &replay);
}

/// 抢占幂等：二次封存不重复生成 L1（会话只封存一次）。
#[tokio::test]
async fn seal_second_call_does_not_regenerate_l1() {
    let env = env_with_l1_reply("seal-idempotent")
        .await
        .expect("封存对照环境应可构建");

    let session_id = fixture(&env).await.expect("fixture 应造数成功");
    let first = env.engine().seal(session_id).await.expect("首次封存应成功");
    assert!(first.sealed, "首次封存应抢到关闭权");

    let second = env
        .engine()
        .seal(session_id)
        .await
        .expect("二次封存应正常返回（未抢到）");
    assert!(!second.sealed, "二次封存不应抢到关闭权");
    assert_eq!(second.l1_count, 0, "二次封存不应生成 L1");

    let l1_count = env
        .storage()
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功")
        .len();
    assert_eq!(l1_count, 1, "库中应只有一条 L1（抢占幂等）");

    env.cleanup().await;
}

// =========================================================
// 手动关闭产物场景（完整封存钩子链）
// =========================================================

/// 发一条消息建立会话 → 注册完整封存钩子链 → 封存；返回会话 id。
async fn send_and_seal(env: &ParityEnv) -> ParityResult<Uuid> {
    fixtures::seed_persona(env.storage(), PERSONA).await?;

    let outcome = env
        .engine()
        .chat_send(ChatSendRequest {
            message: MANUAL_CLOSE_MESSAGE.to_string(),
            persona: Some(PERSONA.to_string()),
            session_id: None,
            conversation_id: None,
            channel: CHANNEL_LOCAL.to_string(),
        })
        .await
        .map_err(|e| ParityError::env("服务装配发送对照消息", e))?;

    env.engine()
        .set_seal_hooks(ramaria_service::full_seal_hooks(env.engine().as_ref()));
    env.engine()
        .seal(outcome.session_id)
        .await
        .map_err(|e| ParityError::env("服务装配封存对照会话", e))?;
    Ok(outcome.session_id)
}

/// 读取封存产物的可观测状态（只取数据库可观测项）。
async fn read_seal_state(
    storage: &SqliteStorage,
    session_id: Uuid,
) -> ParityResult<serde_json::Value> {
    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .map_err(|e| ParityError::env("读取封存后的 L1", e))?;
    let session = storage
        .get_session(session_id)
        .await
        .map_err(|e| ParityError::env("读取封存后的会话", e))?
        .ok_or_else(|| ParityError::env("读取封存后的会话", "会话应存在"))?;
    let message_count = storage
        .list_messages(session_id)
        .await
        .map_err(|e| ParityError::env("读取会话消息", e))?
        .len();
    let pending_jobs = storage
        .list_pending_jobs()
        .await
        .map_err(|e| ParityError::env("读取待定后台任务", e))?;
    let utt_count = storage
        .list_utt_blocks_by_persona(PERSONA)
        .await
        .map_err(|e| ParityError::env("读取 utt 话语块", e))?
        .len();
    let example_count = storage
        .list_all_examples(PERSONA)
        .await
        .map_err(|e| ParityError::env("读取对话示例", e))?
        .len();

    let l1_snapshot: Vec<serde_json::Value> = l1_list
        .iter()
        .map(|l1| {
            json!({
                "summary": l1.summary.clone(),
                "keywords": l1.keywords.clone(),
                "time_period": l1.time_period.clone(),
                "atmosphere": l1.atmosphere.clone(),
                "valence": l1.valence,
                "salience": l1.salience,
                "situation_strength": l1.situation_strength,
                "persona_uid": l1.persona_uid.clone(),
            })
        })
        .collect();

    Ok(json!({
        "session": {
            "ended": session.ended_at.is_some(),
            "message_count": message_count,
        },
        "l1": l1_snapshot,
        "utt_block_count": utt_count,
        "example_count": example_count,
        "pending_job_types": pending_jobs
            .iter()
            .map(|(_, job_type, _)| job_type.clone())
            .collect::<Vec<_>>(),
    }))
}

/// 基线一致：手动关闭（完整封存钩子链）的可观测产物与冻结基线逐字段一致。
///
/// 口径说明:
/// - fixture：同一人格、同一轮消息与同一脚本回复序列（助手回复 → L1 摘要 JSON）；
/// - 由生成入口建立会话后显式封存，覆盖"生成 → 封存 → L1 / utt / 示例 / 待定任务"完整链路；
/// - 快照只取数据库可观测产物（会话结束 / L1 字段 / utt / 示例 / 待定任务），不落 id 与时间戳。
#[tokio::test]
async fn seal_manual_close_products_match_golden_baseline() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::replies(&[
        DEFAULT_ASSISTANT_REPLY,
        L1_JSON_REPLY,
    ]));
    let env = ParityEnv::with_llm("seal-manual-close", llm)
        .await
        .expect("封存对照环境应可构建");
    let session = send_and_seal(&env)
        .await
        .expect("手动关闭封存场景应执行成功");
    let value = read_seal_state(env.storage(), session)
        .await
        .expect("封存产物应可读取");
    let snapshot = Snapshot::new(MANUAL_CLOSE_SCENARIO, value);

    // 关键行为锚点：会话已关闭、消息完整落库、短会话生成单条 L1（观测面成立）
    assert_eq!(
        snapshot.value()["session"]["ended"].as_bool(),
        Some(true),
        "封存后会话应已关闭"
    );
    assert_eq!(
        snapshot.value()["session"]["message_count"].as_u64(),
        Some(2),
        "一轮消息应落库用户消息与助手回复两条"
    );
    assert_eq!(
        snapshot.value()["l1"].as_array().map(Vec::len),
        Some(1),
        "短会话应生成单条 L1"
    );

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );

    env.cleanup().await;
}
