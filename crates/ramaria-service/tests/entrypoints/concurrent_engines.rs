//! crates/ramaria-service/tests/entrypoints/concurrent_engines.rs - 三入口形态同库并发用例
//!
//! 设计特点:
//! - 桌面形态（完整封存钩子链 + 生命周期容器）/ MCP 形态（轻量钩子链 + 空闲循环）/
//!   CLI 形态（默认装配、无后台循环）在同一真实 SQLite 库上并发执行封存 / 写入 / 召回
//! - 并发封存由条件更新抢占收口：断言"每个会话恰好一份 L1"，不以谁抢到为准
//! - CLI 写入会话采用独立脚本回复，召回断言精确落到写入产物对应的 L1
//! - 等待一律轮询 + 超时，不依赖固定 sleep

use std::sync::Arc;
use std::time::Duration;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{EmbeddingProvider, StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig};
use ramaria_service::types::{RecallLayer, RecallRequest};
use ramaria_service::{IdleLoopOptions, LifecycleOptions, default_seal_hooks, full_seal_hooks};

use crate::support::{
    DeterministicEmbedding, ScriptedLlm, TestDb, drain_stream, seed_timed_out_session,
    stream_request, wait_until,
};

/// 脚本回复：其它会话的摘要（中性文本，不参与写入会话的召回断言）。
const NEUTRAL_L1_JSON: &str = r#"{
  "summary": "用户聊了聊近况，气氛轻松。",
  "keywords": "近况,闲聊",
  "time_period": "下午",
  "atmosphere": "轻松",
  "valence": 0.2,
  "salience": 0.5,
  "situation_strength": 2
}"#;

/// 脚本回复：CLI 写入会话的摘要（含检索特征词"桨板"）。
const CLI_L1_JSON: &str = r#"{
  "summary": "用户周末去西湖划桨板，玩得很开心。",
  "keywords": "桨板,西湖",
  "time_period": "周末",
  "atmosphere": "愉快",
  "valence": 0.7,
  "salience": 0.6,
  "situation_strength": 3
}"#;

/// 三入口形态并发封存 / 写入 / 召回：每个会话恰好一份 L1，写入内容经另一引擎可召回。
#[tokio::test]
async fn three_entry_shapes_share_one_db_and_seal_each_session_once() {
    const PERSONA: &str = "char-entry-concurrent";
    const WRITE_MESSAGE: &str = "我周末去西湖划桨板了";

    let db = TestDb::new("entry-concurrent");
    let mut config = RamariaConfig::default();
    // 测试加速：不等待批量 LLM 请求间节流
    config.thresholds.cluster_delay_ms = 0;
    // 保持 L1 处于未吸收（可检索）态：避免封存钩子触发 L2 计数路径
    config.thresholds.l2_trigger_count = 100;
    let idle_minutes = config.session.l1_idle_minutes;

    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());

    // ---- 桌面形态：完整封存钩子链 + 生命周期容器（空闲检查 + L2/L3 调度 + 启动补扫） ----
    let (desktop, desktop_storage) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(NEUTRAL_L1_JSON)),
            Some(Arc::clone(&embedding)),
            config.clone(),
        )
        .await
        .expect("桌面形态引擎应可装配");
    desktop.set_seal_hooks(full_seal_hooks(desktop.as_ref()));
    let lifecycle = desktop.start_lifecycle(
        LifecycleOptions::desktop()
            .with_idle_interval(1)
            .with_l2_l3_interval(1)
            .with_l2_l3_first_delay(1),
    );

    // ---- MCP 形态：轻量封存钩子链 + 空闲循环（IdleLoop） ----
    let (mcp, _) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(NEUTRAL_L1_JSON)),
            Some(Arc::clone(&embedding)),
            config.clone(),
        )
        .await
        .expect("MCP 形态引擎应可装配");
    mcp.set_seal_hooks(default_seal_hooks(mcp.as_ref()));
    let mut idle_loop = mcp.spawn_idle_loop_with(IdleLoopOptions::new(1));

    // ---- CLI 形态：默认装配（无封存钩子、无后台循环） ----
    let (cli, _) = db
        .open_engine(
            Arc::new(ScriptedLlm::reply(CLI_L1_JSON)),
            Some(Arc::clone(&embedding)),
            config.clone(),
        )
        .await
        .expect("CLI 形态引擎应可装配");

    // ---- 种子：人格 + 三个超时会话 + 就绪推进（嵌入可用 → Ready） ----
    crate::support::fixtures::seed_persona(desktop_storage.as_ref(), PERSONA)
        .await
        .expect("种子人格应写入成功");
    desktop_storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .expect("后端配置应写入成功");
    // 显式标记索引已构建：本用例聚焦三入口并发，不覆盖索引构建链路
    desktop_storage
        .set_index_version(1)
        .await
        .expect("写入索引版本应成功");
    let session_a = seed_timed_out_session(desktop_storage.as_ref(), PERSONA, idle_minutes)
        .await
        .expect("超时会话 A 应造数成功");
    let session_b = seed_timed_out_session(desktop_storage.as_ref(), PERSONA, idle_minutes)
        .await
        .expect("超时会话 B 应造数成功");
    let session_c = seed_timed_out_session(desktop_storage.as_ref(), PERSONA, idle_minutes)
        .await
        .expect("超时会话 C 应造数成功");
    for (label, engine) in [("桌面", &desktop), ("MCP", &mcp), ("CLI", &cli)] {
        let state = engine
            .refresh_setup_state()
            .await
            .unwrap_or_else(|e| panic!("{label} 形态刷新状态应成功: {e}"));
        assert_eq!(
            state,
            AppState::Ready,
            "{label} 形态：后端配置齐备 + 嵌入可用时应为 Ready"
        );
    }

    // ---- 并发动作：桌面空闲检查 / MCP 空闲检查 / CLI 手动封存 / CLI 流式写入 ----
    let cli_write = async {
        let handle = cli
            .chat_stream(stream_request(WRITE_MESSAGE, PERSONA))
            .await
            .expect("CLI 写入应返回事件流句柄");
        drain_stream(handle).await
    };
    let (tick_desktop, tick_mcp, seal_c, write_summary) = tokio::join!(
        lifecycle.tick_idle(),
        mcp.tick_idle(),
        cli.seal(session_c),
        cli_write,
    );
    tick_desktop.expect("桌面空闲检查应成功");
    tick_mcp.expect("MCP 空闲检查应成功");
    seal_c.expect("CLI 手动封存应成功（抢到与否均为合法结果）");
    let d_session = write_summary.session_id;
    assert_eq!(
        write_summary.error.as_deref(),
        None,
        "CLI 写入事件流不应报错（session={d_session}）"
    );
    assert!(
        write_summary.done,
        "CLI 写入事件流应正常结束（session={d_session}）"
    );

    // ---- CLI 收尾：等待写入落库后封存该会话（CLI 手动保存语义） ----
    wait_until(
        "CLI 写入的会话消息落库",
        Duration::from_secs(5),
        || {
            let storage = Arc::clone(&desktop_storage);
            async move {
                storage
                    .list_messages(d_session)
                    .await
                    .map(|messages| messages.len() >= 2)
                    .unwrap_or(false)
            }
        },
    )
    .await;
    let seal_d = cli.seal(d_session).await.expect("CLI 封存写入会话应成功");
    assert!(
        seal_d.sealed,
        "写入会话无竞争，应抢到封存权（session={d_session}）"
    );
    assert_eq!(
        seal_d.l1_count, 1,
        "写入会话应生成一条 L1（session={d_session}）"
    );

    // ---- 轮询等待三个超时会话收敛（循环与手动封存共同收敛；抢占先于摘要落库） ----
    let targets = [session_a, session_b, session_c];
    wait_until(
        "三个超时会话封存完成且各有一份 L1",
        Duration::from_secs(10),
        || {
            let storage = Arc::clone(&desktop_storage);
            async move {
                for session in targets {
                    let Ok(Some(row)) = storage.get_session(session).await else {
                        return false;
                    };
                    if row.ended_at.is_none() {
                        return false;
                    }
                    match storage.list_memory_l1(session).await {
                        Ok(list) if list.len() == 1 => {}
                        _ => return false,
                    }
                }
                true
            }
        },
    )
    .await;

    // ---- 断言：四个会话各恰好一份 L1（原子抢占去重） ----
    for (label, session) in [
        ("A", session_a),
        ("B", session_b),
        ("C", session_c),
        ("CLI 写入", d_session),
    ] {
        let l1_count = desktop_storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功")
            .len();
        assert_eq!(
            l1_count, 1,
            "会话 {label}（{session}）应恰好一份 L1（并发封存抢占去重）"
        );
    }

    // ---- 断言：CLI 写入的会话摘要经桌面形态引擎可召回（同库写入 → 另一入口召回一致） ----
    let d_l1 = desktop_storage
        .list_memory_l1(d_session)
        .await
        .expect("读取写入会话 L1 应成功");
    let expected_id = format!("L1:{}", d_l1[0].id);
    let result = desktop
        .recall(RecallRequest {
            query: Some("桨板".to_string()),
            persona: Some(PERSONA.to_string()),
            include: Some(vec![RecallLayer::L1]),
            max_items: Some(10),
            ..RecallRequest::default()
        })
        .await
        .expect("桌面召回应成功");
    assert!(
        result
            .items
            .iter()
            .any(|item| item.layer == RecallLayer::L1 && item.id == expected_id),
        "桌面召回应命中 CLI 写入会话的摘要（session={d_session}，期望 id={expected_id}）: {:?}",
        result.items
    );

    // ---- 收尾：优雅关停两个后台循环，清理测试库 ----
    lifecycle.shutdown().await;
    idle_loop.shutdown().await;
    db.cleanup().await;
}
