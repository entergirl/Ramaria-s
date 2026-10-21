//! crates/ramaria-service/src/index/tests.rs - Ramaria 检索索引构建与增量镜像测试
//!
//! 设计特点:
//! - 由 index.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖懒加载 / 显式重建与冷却 /
//!   原子替换与失败告警 / 代次刷新 / 增量镜像 / BM25 词典增强迁移六条路径
//! - 真实 SQLite（临时文件库 + 全量 migration）与确定性 mock 嵌入 / mock LLM：
//!   直接断言检索命中、镜像落库与版本标记口径
//! - 存储故障路径经可注入故障的夹具（`engine_with_failable_storage`）驱动：
//!   验证重建失败告警位的脱敏记录、保留与恢复清除
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::*;
use crate::test_support::{
    CountingEmbedding, DeterministicEmbedding, MockLlm, engine_with_db,
    engine_with_failable_storage, engine_with_llm_and_config, engine_with_llm_config_and_embedding,
    seed_l1 as seed_l1_raw, seed_persona,
};
use crate::types::{RecallLayer, RecallRequest, RecallResult};
use ramaria_core::config::RamariaConfig;
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::{
    EmbeddingProvider, SETTING_BM25_INDEX_VERSION, StoreCrud, StoreInfrastructure,
};
use ramaria_core::types::{
    EventRelation, EventRelationKind, IndexStatus, MemoryEvent, MemoryVector, Message, MessageRole,
    MessageSource, now_ms,
};
use ramaria_memory::retriever::{SearchRequest, SearchResult};
use ramaria_storage::SqliteStorage;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// 造一条 L1（带"工作压力"关键词，便于关键词镜像/BM25 命中）。
async fn seed_l1(storage: &SqliteStorage, persona: &str, summary: &str) -> Uuid {
    seed_l1_raw(storage, persona, summary, Some("工作压力"), 1_000).await
}

/// 以 L1 层检索模式召回一句（测试统一口径：只关心 L1 是否命中）。
async fn recall_l1(engine: &Engine, query: &str) -> RecallResult {
    engine
        .recall(RecallRequest {
            query: Some(query.to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功")
}

/// 直接读取已构建检索器上的检索结果（不经过 recall 的 persona 过滤与镜像通道）。
fn search_docs(engine: &Engine, query: &str) -> Vec<SearchResult> {
    let slot = engine.retriever_slot();
    let guard = read_recover(&*slot, "index.retriever_slot");
    guard.as_ref().expect("索引应已构建").search(
        &SearchRequest {
            query: query.to_string(),
            persona_uid: None,
            top_k: 10,
            filter_share: false,
        },
        None,
    )
}

/// 懒加载：首次构建返回 true，重复调用返回 false；索引可命中。
#[tokio::test]
async fn ensure_loaded_builds_once_and_searches() {
    let (engine, storage, dir) = engine_with_db("index").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户最近工作压力很大，常常加班").await;

    assert!(engine.ensure_index_loaded().await.expect("加载成功"));
    assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));
    assert!(engine.is_retriever_loaded());

    let result = engine
        .recall(RecallRequest {
            query: Some("工作压力".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(!result.items.is_empty(), "懒加载后应能检索到 L1");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引加载幂等：首次构建返回 true，已加载后重复调用走早退分支返回 false。
#[tokio::test]
async fn ensure_loaded_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("index-version").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户最近工作压力很大，常常加班").await;

    assert!(engine.ensure_index_loaded().await.expect("加载成功"));
    assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引未加载时不报错：召回返回空结果（首次召回会自动加载，此处直接调用共用实现）。
#[tokio::test]
async fn recall_without_load_still_succeeds() {
    let (engine, storage, dir) = engine_with_db("index-empty").await;
    seed_persona(&storage, "char-0001").await;

    // 未显式加载 → recall 内部会懒加载（行为：能召回已入库内容）
    let result = engine
        .recall(RecallRequest {
            query: Some("任意".to_string()),
            persona: Some("char-0001".to_string()),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(result.items.is_empty(), "库中无记忆时返回空结果");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 增量镜像：加载后新写入的 L1 经 `index_l1_into_mirrors` 即可被检索命中。
#[tokio::test]
async fn incremental_l1_is_searchable() {
    let (engine, storage, dir) = engine_with_db("index-incremental").await;
    seed_persona(&storage, "char-0001").await;
    engine.ensure_index_loaded().await.expect("加载成功");

    // 新 L1：先写库，再走增量镜像（与封存路径一致）
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话");
    let mut l1 = ramaria_core::types::MemoryL1::new(
        session.id,
        "用户这周开始学习攀岩，周末去了岩馆".to_string(),
        None,
    );
    l1.persona_uid = Some("char-0001".to_string());
    l1.keywords = Some("攀岩".to_string());
    storage.save_memory_l1(&l1).await.expect("写入 L1");
    index_l1_into_mirrors(&engine, &l1).await;

    let result = engine
        .recall(RecallRequest {
            query: Some("攀岩".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(
        result.items.iter().any(|i| i.text.contains("攀岩")),
        "增量 L1 应立即可检索: {:?}",
        result.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 开销口径：增量镜像对每条 L1 恰好一次批量词条状态查询（3 个关键词 1 次，不逐词）。
#[tokio::test]
async fn incremental_l1_mirror_queries_keyword_status_once_per_l1() {
    let (engine, storage, failable, dir) =
        engine_with_failable_storage("index-keyword-status-count").await;
    seed_persona(&storage, "char-0001").await;

    let mut l1s = Vec::new();
    for i in 0..3 {
        let id = seed_l1_raw(
            &storage,
            "char-0001",
            &format!("计数口径摘要 {i}"),
            Some("睡前,阅读,复盘"),
            1_000 + i,
        )
        .await;
        let l1 = storage
            .get_memory_l1(id)
            .await
            .expect("读取 L1 应成功")
            .expect("L1 应存在");
        l1s.push(l1);
    }

    assert_eq!(
        failable.keyword_status_query_count(),
        0,
        "增量镜像前不应有词条状态查询"
    );
    for l1 in &l1s {
        index_l1_into_mirrors(&engine, l1).await;
    }
    assert_eq!(
        failable.keyword_status_query_count(),
        3,
        "每条 L1 一次批量状态查询（3 个关键词 1 次，不逐词）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引未加载期间产生的 L1：置脏标记 → 下次加载重建并找回该 L1（不漏检索）。
#[tokio::test]
async fn dirty_flag_forces_rebuild_after_incremental_write() {
    let (engine, storage, dir) = engine_with_db("index-dirty").await;
    seed_persona(&storage, "char-0001").await;

    // 未加载就发生增量（模拟"加载窗口内的封存"）
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话");
    let mut l1 =
        ramaria_core::types::MemoryL1::new(session.id, "用户这周开始学习攀岩".to_string(), None);
    l1.persona_uid = Some("char-0001".to_string());
    l1.keywords = Some("攀岩".to_string());
    storage.save_memory_l1(&l1).await.expect("写入 L1");

    index_l1_into_mirrors(&engine, &l1).await;
    assert!(engine.index_dirty(), "未加载时的增量应置脏");
    assert!(!engine.is_retriever_loaded());

    // 因脏标记，加载必须构建（不能因"已加载"提前返回）→ L1 可检索
    assert!(
        engine.ensure_index_loaded().await.expect("加载成功"),
        "脏标记应触发构建"
    );
    assert!(!engine.index_dirty(), "构建后脏标记应清除");

    let result = engine
        .recall(RecallRequest {
            query: Some("攀岩".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(
        result.items.iter().any(|i| i.text.contains("攀岩")),
        "加载窗口内的 L1 不应漏检索: {:?}",
        result.items
    );

    // 无脏标记 → 不重复构建
    assert!(!engine.ensure_index_loaded().await.expect("重复加载成功"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无主 L1（persona_uid IS NULL）也会被加载（导入数据可检索）。
#[tokio::test]
async fn unbound_l1_is_loaded() {
    let (engine, storage, dir) = engine_with_db("index-unbound").await;
    seed_persona(&storage, "char-0001").await;
    // 无主 L1：不绑定 persona（persona_uid 保持 None）
    let session = storage.create_session(None).await.expect("创建会话");
    let l1 = ramaria_core::types::MemoryL1::new(
        session.id,
        "导入的聊天记录提到喜欢喝咖啡".to_string(),
        None,
    );
    storage.save_memory_l1(&l1).await.expect("写入 L1");

    // 会话消息（避免会话被视为空壳；无主 L1 由 unbound 通道加载）
    let message = Message::new(
        session.id,
        MessageRole::User,
        "导入消息".to_string(),
        MessageSource::Local,
    );
    storage.save_message(&message).await.expect("写入消息");

    engine.ensure_index_loaded().await.expect("加载成功");
    let result = engine
        .recall(RecallRequest {
            query: Some("咖啡".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("召回成功");
    assert!(
        result.items.iter().any(|i| i.text.contains("咖啡")),
        "无主 L1 应可检索: {:?}",
        result.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 代次刷新（跨进程场景）：另一连接写入新 L1 后，本引擎的已加载索引能感知并刷新。
#[tokio::test]
async fn cross_process_write_refreshes_index() {
    let (engine, storage, dir) = engine_with_db("index-cross").await;
    seed_persona(&storage, "char-0001").await;
    engine.ensure_index_loaded().await.expect("加载成功");

    // 库内无变化 → 代次一致，不重复构建
    assert!(
        !engine.ensure_index_loaded().await.expect("重复加载成功"),
        "无变化时不应重建索引"
    );

    // 另一"进程"视角：同一库文件上的第二个存储句柄写入新 L1
    let db_path = dir.join("ramaria.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("第二连接池应可创建");
    let other_storage = SqliteStorage::new(pool);
    let session = other_storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话");
    let mut l1 = ramaria_core::types::MemoryL1::new(
        session.id,
        "用户最近迷上了夜跑，每周三次".to_string(),
        None,
    );
    l1.persona_uid = Some("char-0001".to_string());
    l1.keywords = Some("夜跑".to_string());
    other_storage.save_memory_l1(&l1).await.expect("写入 L1");

    // 本引擎召回：语料戳变化触发刷新，新记忆必须可见
    let result = recall_l1(&engine, "夜跑").await;
    assert!(
        result.items.iter().any(|i| i.text.contains("夜跑")),
        "跨进程写入的新 L1 应可检索: {:?}",
        result.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 降级链：无嵌入模型时不阻塞索引构建，BM25 / 关键词通道命中，向量通道缺席。
#[tokio::test]
async fn no_embedding_degrades_to_non_vector_channels() {
    let (engine, storage, dir) = engine_with_db("index-degraded").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户最近开始学习游泳，每周去两次").await;

    // 测试脚手架不注入嵌入模型：向量通道不可用（真实进程对应的"模型缺失"降级场景）
    assert!(
        !engine.is_embedding_available(),
        "无嵌入模型时应走降级链（不阻塞装配与索引构建）"
    );
    assert!(
        engine
            .ensure_index_loaded()
            .await
            .expect("降级路径也应构建成功"),
        "首次召回应完成懒加载构建"
    );
    assert!(engine.is_retriever_loaded());

    let result = recall_l1(&engine, "游泳").await;
    assert!(
        !result.items.is_empty(),
        "嵌入缺失时 BM25 / 关键词镜像应仍能命中: {:?}",
        result.items
    );
    assert_eq!(
        result.stats.channels.get("vector").copied().unwrap_or(0),
        0,
        "无嵌入模型时向量通道不得有命中（统计口径）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 刷新间隔：冷却窗口内检测到跨进程写入不重建（沿用现有索引），窗口过后的下次召回补上。
#[tokio::test]
async fn refresh_interval_defers_rebuild_within_cooldown() {
    // 冷却窗口取 1 秒（配置项单位秒；生产默认 0 = 不节流，即时可见）
    let mut config = RamariaConfig::default();
    config.index.refresh_interval_seconds = 1;
    let (engine, storage, dir) =
        engine_with_llm_and_config("index-refresh", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;

    assert!(
        engine.ensure_index_loaded().await.expect("首次加载成功"),
        "首次召回应完成懒加载构建"
    );
    // 首次懒加载不受冷却约束（构建照常发生），但构建完成后即进入冷却窗口
    assert!(
        !engine.index_rebuild_cooldown_elapsed(),
        "刚构建完成应处于冷却窗口内（窗口从构建完成起算）"
    );

    // 另一"进程"写入新 L1（语料代次变化）：必须在同一库文件的第二个连接上写入，
    // 模拟"桌面写、MCP 读"的跨进程场景
    let db_path = dir.join("ramaria.db");
    let other = SqliteStorage::new(
        ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("第二连接池应可创建"),
    );
    seed_l1_raw(
        &other,
        "char-0001",
        "用户最近迷上了夜跑",
        Some("夜跑"),
        2_000,
    )
    .await;
    assert!(
        !engine.index_rebuild_cooldown_elapsed(),
        "构建后 1 秒内应处于冷却窗口"
    );

    // 冷却窗口内：不重建 → 沿用现有索引，新 L1 尚不可见
    let within = recall_l1(&engine, "夜跑").await;
    assert!(
        within.items.is_empty(),
        "冷却窗口内应沿用现有索引（抑制重建风暴）: {:?}",
        within.items
    );

    // 窗口过后：下一次召回补上重建 → 新 L1 可见
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        engine.index_rebuild_cooldown_elapsed(),
        "超过刷新间隔后应允许重建"
    );
    let after = recall_l1(&engine, "夜跑").await;
    assert!(
        after.items.iter().any(|i| i.text.contains("夜跑")),
        "窗口过后的召回应重建并命中新 L1: {:?}",
        after.items
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 显式重建加载无主 L1（persona_uid IS NULL）：导入数据重建后进入索引。
#[tokio::test]
async fn rebuild_loads_unbound_l1() {
    let (engine, storage, dir) = engine_with_db("index-rebuild-unbound").await;
    seed_persona(&storage, "char-0001").await;
    // 无主 L1：不绑定 persona（persona_uid 保持 None）
    let session = storage.create_session(None).await.expect("创建会话");
    let l1 = ramaria_core::types::MemoryL1::new(
        session.id,
        "用户喜欢喝咖啡，每天上午必点一杯拿铁".to_string(),
        None,
    );
    storage.save_memory_l1(&l1).await.expect("写入 L1");

    let total = engine.rebuild_index().await.expect("重建应成功");
    assert!(total >= 1, "无主 L1 必须被加载进索引，实际 total={total}");
    let slot = engine.retriever_slot();
    let guard = read_recover(&*slot, "index.retriever_slot");
    assert!(
        guard.as_ref().expect("索引应已构建").doc_count() >= 1,
        "检索器 doc_count 应为 ≥1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 显式重建返回 L1 + L2 文档总数。
#[tokio::test]
async fn rebuild_index_returns_document_total() {
    let (engine, storage, dir) = engine_with_db("index-rebuild-total").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户喜欢喝咖啡").await;
    seed_l1(&storage, "char-0001", "用户最近开始学习游泳").await;

    let total = engine.rebuild_index().await.expect("重建应成功");
    assert_eq!(total, 2, "返回值应为 L1 + L2 视图总数");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 图谱构建：事件为实体节点、事件关系为边 — 检索产出图谱命中，
/// 且命中携带 1-hop 关联事件标题。
#[tokio::test]
async fn rebuild_builds_graph_from_event_relations() {
    let (engine, storage, dir) = engine_with_db("index-graph").await;
    seed_persona(&storage, "char-0001").await;

    // 两个事件：标题即图谱实体名（含可检索的独特词）
    let first = MemoryEvent::new(
        "char-0001".to_string(),
        "陶艺展".to_string(),
        "用户周末去看了陶艺展".to_string(),
        1_000,
        2_000,
    );
    let first_id = storage.save_event(&first).await.expect("写入事件应成功");
    let second = MemoryEvent::new(
        "char-0001".to_string(),
        "陶艺课".to_string(),
        "用户报名了陶艺课".to_string(),
        3_000,
        4_000,
    );
    let second_id = storage.save_event(&second).await.expect("写入事件应成功");
    storage
        .save_event_relation(&EventRelation::new(
            first_id,
            second_id,
            EventRelationKind::RelatedTo,
        ))
        .await
        .expect("写入事件关系应成功");

    engine.rebuild_index().await.expect("重建应成功");

    // 公开检索入口：图谱通道产出命中（实体名以 "[图谱实体] 标题" 呈现）
    let hits = search_docs(&engine, "陶艺");
    assert!(
        hits.iter().any(|r| r.layer == "graph"
            && r.graph_score.is_some()
            && r.doc_summary.contains("陶艺展")),
        "重建后检索应产出图谱通道命中: {hits:?}"
    );

    // 图谱内部检视：节点 / 边计数与 1-hop 关联关系
    let slot = engine.retriever_slot();
    let mut guard = write_recover(&*slot, "index.retriever_slot");
    let retriever = guard.as_mut().expect("索引应已构建");
    let graph_config = retriever.config().graph.clone();
    let graph = retriever.graph_mut();
    assert_eq!(graph.node_count(), 2, "两个事件应各成一个图节点");
    assert_eq!(graph.edge_count(), 1, "一条事件关系应成一条边");
    let first_key = ramaria_memory::graph_retriever::graph_entity_key(Some("char-0001"), "陶艺展");
    let second_key = ramaria_memory::graph_retriever::graph_entity_key(Some("char-0001"), "陶艺课");
    let graph_hits = graph.search("陶艺", &graph_config);
    assert!(
        graph_hits.iter().any(|h| {
            h.entity_name == first_key && h.related_entities.iter().any(|name| name == &second_key)
        }),
        "图谱命中应含 1-hop 关联事件标题: {graph_hits:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 无事件关系时重建正常完成：事件仍成孤立节点、图无边（关系为空不阻塞构建）。
#[tokio::test]
async fn rebuild_without_relations_builds_isolated_graph_nodes() {
    let (engine, storage, dir) = engine_with_db("index-graph-norel").await;
    seed_persona(&storage, "char-0001").await;
    let event = MemoryEvent::new(
        "char-0001".to_string(),
        "独自散步".to_string(),
        "用户在河边散步".to_string(),
        1_000,
        2_000,
    );
    storage.save_event(&event).await.expect("写入事件应成功");

    engine.rebuild_index().await.expect("无关系时重建应成功");

    let slot = engine.retriever_slot();
    let mut guard = write_recover(&*slot, "index.retriever_slot");
    let graph = guard.as_mut().expect("索引应已构建").graph_mut();
    assert_eq!(graph.node_count(), 1, "无关系的事件仍应成节点");
    assert_eq!(graph.edge_count(), 0, "无关系时图不应有边");

    let _ = std::fs::remove_dir_all(&dir);
}

/// BM25 词典增强分词迁移：词表就绪后重建切为词典增强口径并写回版本标记。
///
/// 步骤:
/// 1. 分词版本缺失 + 词表为空 → 重建后不写版本标记；纯 bigram 口径下跨词噪声
///    （"作压"）能命中（旧口径基线）；
/// 2. 注入已确认规范词 → 再次重建：版本标记写为当前版本；整词查询命中；
///    跨词噪声不再命中。
#[tokio::test]
async fn bm25_dictionary_migration_upgrades_and_removes_noise() {
    let (engine, storage, dir) = engine_with_db("index-bm25-migration").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "最近工作压力很大常常加班").await;

    // 1) 词表为空：不升级版本标记（重建与旧版等价），纯 bigram 口径可检索
    engine.rebuild_index().await.expect("首次重建应成功");
    let setting_before = storage
        .get_setting(SETTING_BM25_INDEX_VERSION)
        .await
        .expect("读取设置应成功");
    assert!(
        setting_before.is_none(),
        "词表为空时不应写入版本标记，实际 {setting_before:?}"
    );
    assert!(
        !search_docs(&engine, "作压").is_empty(),
        "迁移前旧索引（纯 bigram）应可检索：'作压' 噪声命中为旧口径基线"
    );

    // 2) 词表就绪：再次重建触发迁移
    storage
        .upsert_keyword("工作压力")
        .await
        .expect("写入规范词应成功");
    engine.rebuild_index().await.expect("迁移重建应成功");
    let setting_after = storage
        .get_setting(SETTING_BM25_INDEX_VERSION)
        .await
        .expect("读取设置应成功");
    assert_eq!(
        setting_after.as_deref(),
        Some("2"),
        "迁移完成后版本标记应为当前版本 2"
    );
    assert!(
        search_docs(&engine, "工作压力")
            .iter()
            .any(|r| r.doc_summary.contains("工作压力")),
        "词典整词查询应命中文档（索引可检索）"
    );
    assert!(
        search_docs(&engine, "作压").is_empty(),
        "词典口径下跨词噪声 '作压' 不应命中"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重建后关键词镜像与加载文档一致；镜像维护不影响检索器：
/// 镜像被外部清空不改变检索结果，再次重建恢复（幂等收敛）。
#[tokio::test]
async fn rebuild_syncs_keyword_service_mirror_and_preserves_search() {
    let (engine, storage, dir) = engine_with_db("index-mirror").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户喜欢喝咖啡，每天上午必点一杯拿铁",
    )
    .await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;

    let total = engine.rebuild_index().await.expect("重建应成功");
    assert!(total >= 2, "应加载 ≥2 条 L1，实际 {total}");

    // 镜像与加载文档一致（doc_count 级）
    let mirror = engine.keyword_mirror();
    {
        let guard = read_recover(&*mirror, "index.keyword_mirror");
        assert_eq!(guard.doc_count(), total, "镜像文档数应与重建加载数一致");
    }

    // 镜像维护不影响既有检索：镜像清空前后 search 结果一致
    let search_summaries = |engine: &Engine| -> Vec<String> {
        search_docs(engine, "工作压力")
            .into_iter()
            .map(|r| r.doc_summary)
            .collect()
    };
    let before = search_summaries(&engine);
    assert!(!before.is_empty(), "对照检索应命中既有 L1");
    {
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.clear_docs(); // 模拟镜像被外部误操作清空
    }
    let after = search_summaries(&engine);
    assert_eq!(before, after, "镜像操作不得改变 Retriever 检索结果");

    // 再次重建 → 镜像恢复与视图一致（幂等收敛）
    let total2 = engine.rebuild_index().await.expect("再次重建应成功");
    assert_eq!(total2, total);
    {
        let guard = read_recover(&*mirror, "index.keyword_mirror");
        assert_eq!(guard.doc_count(), total2, "再次重建后镜像文档数应恢复");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 嵌入可用 + 词典非空 → 重建后关键词镜像挂载 Fuzzy 语义层（可用分支）。
#[tokio::test]
async fn rebuild_with_embedding_and_pool_mounts_fuzzy() {
    let embedding: Option<Arc<dyn EmbeddingProvider>> =
        Some(Arc::new(DeterministicEmbedding::new()));
    let (engine, storage, dir) = engine_with_llm_config_and_embedding(
        "index-fuzzy",
        MockLlm::local(),
        RamariaConfig::default(),
        embedding,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户最近工作压力很大，常常加班到深夜",
    )
    .await;
    storage
        .upsert_keyword("工作压力")
        .await
        .expect("写入规范词应成功");

    engine.rebuild_index().await.expect("重建应成功");
    let mirror = engine.keyword_mirror();
    let guard = read_recover(&*mirror, "index.keyword_mirror");
    assert!(guard.pool_len() >= 1, "词典池应装载注入的规范词");
    let fuzzy = guard
        .composite()
        .fuzzy()
        .expect("嵌入可用时应挂载 Fuzzy 层");
    assert!(fuzzy.is_ready());

    let _ = std::fs::remove_dir_all(&dir);
}

/// core `[retrieval]` 配置经重建真实应用进内存检索器
/// （RRF 融合参数 + 向量通道开关，默认配置下行为等价）。
#[tokio::test]
async fn rebuild_applies_core_retrieval_config() {
    let mut config = RamariaConfig::default();
    config.retrieval.rrf_k = 90;
    config.retrieval.bm25_weight = 0.5;
    config.retrieval.graph_weight = 0.4;
    config.retrieval.enable_vector = false;
    let (engine, storage, dir) =
        engine_with_llm_and_config("index-retrieval-config", MockLlm::local(), config).await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "用户喜欢喝咖啡").await;

    engine.rebuild_index().await.expect("重建应成功");
    let slot = engine.retriever_slot();
    let guard = read_recover(&*slot, "index.retriever_slot");
    let retriever = guard.as_ref().expect("索引应已构建");
    assert!(
        !retriever.config().enable_vector,
        "向量通道开关应随重建应用"
    );
    assert_eq!(retriever.config().rrf.k, 90.0, "RRF 平滑系数应随重建应用");
    assert_eq!(retriever.config().rrf.bm25_weight, 0.5);
    assert_eq!(retriever.config().rrf.graph_weight, 0.4);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重建失败（存储读取错误）→ 旧索引保持不变且仍可检索，告警位置位；
/// 恢复后重建成功 → 告警位复位。
#[tokio::test]
async fn rebuild_failure_keeps_old_index_searchable() {
    let (engine, storage, failable, dir) =
        engine_with_failable_storage("index-rebuild-failure").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户喜欢喝咖啡，每天上午必点一杯拿铁",
    )
    .await;

    // 1) 首次重建成功 → 索引可检索、告警位为 false
    engine.rebuild_index().await.expect("首次重建应成功");
    assert!(
        !engine.is_index_rebuild_failed(),
        "重建成功后告警位应为 false"
    );
    let hits_before = search_docs(&engine, "咖啡");
    assert!(!hits_before.is_empty(), "首次重建后应可检索");

    // 2) 注入存储读取失败 → 重建报错、告警位置位
    failable.set_fail_list_personas(true);
    let err = engine
        .rebuild_index()
        .await
        .expect_err("存储读取失败时重建应返回错误");
    assert!(!err.to_string().is_empty(), "错误信息不应为空");
    assert!(engine.is_index_rebuild_failed(), "重建失败应置位告警位");

    // 3) 旧索引原子保留：失败后检索结果与失败前一致（未清空、未半成品）
    let hits_after = search_docs(&engine, "咖啡");
    assert!(!hits_after.is_empty(), "重建失败后旧索引必须仍可检索");
    assert_eq!(hits_before.len(), hits_after.len(), "旧索引文档不应丢失");

    // 4) 恢复后重建成功 → 告警位复位
    failable.set_fail_list_personas(false);
    engine.rebuild_index().await.expect("恢复后重建应成功");
    assert!(!engine.is_index_rebuild_failed(), "重建恢复后告警位应复位");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 构建失败记录：失败置入脱敏原因（保留可诊断关键字、折叠为单行）；恢复成功后清除。
#[tokio::test]
async fn build_failure_record_tracks_and_clears() {
    let (engine, storage, failable, dir) =
        engine_with_failable_storage("index-failure-record").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(
        &storage,
        "char-0001",
        "用户喜欢喝咖啡，每天上午必点一杯拿铁",
    )
    .await;

    // 首次成功：无失败记录
    engine.rebuild_index().await.expect("首次重建应成功");
    assert!(
        engine.index_build_failure().is_none(),
        "构建成功后不得保留失败记录"
    );

    // 失败：置入脱敏原因（保留可诊断关键字、折叠为单行、时间戳有效）
    failable.set_fail_list_personas(true);
    let err = engine
        .rebuild_index()
        .await
        .expect_err("存储读取失败时重建应报错");
    assert!(!err.to_string().is_empty(), "错误信息不应为空");
    let failure = engine.index_build_failure().expect("失败后应有失败记录");
    assert!(
        failure.reason.contains("list_personas"),
        "原因应保留可诊断信息: {}",
        failure.reason
    );
    assert!(
        !failure.reason.contains('\n') && !failure.reason.contains('\r'),
        "原因应折叠为单行: {}",
        failure.reason
    );
    assert!(failure.at_ms > 0, "记录时间应为有效时间戳");

    // 恢复成功：失败记录清除
    failable.set_fail_list_personas(false);
    engine.rebuild_index().await.expect("恢复后重建应成功");
    assert!(
        engine.index_build_failure().is_none(),
        "恢复后应清除失败记录"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// L1 增量镜像：嵌入可用时向量同时写入向量表（指纹 model@dim#size）。
#[tokio::test]
async fn l1_incremental_index_writes_memory_vector() {
    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
    let (engine, storage, dir) = engine_with_llm_config_and_embedding(
        "index-l1-vector",
        MockLlm::local(),
        RamariaConfig::default(),
        Some(embedding),
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    let id = seed_l1(&storage, "char-0001", "用户最近在学游泳，每周去两次").await;
    let l1 = storage
        .get_memory_l1(id)
        .await
        .expect("读取 L1 应成功")
        .expect("L1 应存在");

    super::index_l1_into_mirrors(&engine, &l1).await;

    let fingerprint =
        crate::embedding_store::model_fingerprint(&ramaria_core::traits::EmbeddingModelInfo {
            model_id: "mock-deterministic-embedding".to_string(),
            dimension: DeterministicEmbedding::DIMENSION,
        });
    let rows = engine
        .storage_ref()
        .load_memory_vectors(&fingerprint)
        .await
        .expect("加载向量应成功");
    assert_eq!(rows.len(), 1, "L1 增量应写入一条向量");
    assert_eq!(rows[0].doc_type, ramaria_core::types::VECTOR_DOC_L1);
    assert_eq!(rows[0].doc_id, id.to_string());
    assert_eq!(rows[0].dim as usize, DeterministicEmbedding::DIMENSION);
    assert_eq!(rows[0].vector.len(), DeterministicEmbedding::DIMENSION);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 向量持久化、补算与状态机
// =========================================================

/// 装配一台带计数嵌入 mock 的引擎（测试统一入口）。
async fn engine_with_counting_embedding(
    tag: &str,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, Arc<CountingEmbedding>, PathBuf) {
    let embedding = Arc::new(CountingEmbedding::new());
    let provider: Arc<dyn EmbeddingProvider> = embedding.clone();
    let (engine, storage, dir) =
        engine_with_llm_config_and_embedding(tag, MockLlm::local(), config, Some(provider)).await;
    (engine, storage, embedding, dir)
}

/// 造一条已落库的向量行。
async fn seed_vector(
    storage: &SqliteStorage,
    doc_type: &str,
    doc_id: &str,
    model: &str,
    text: &str,
) {
    let row = MemoryVector {
        doc_type: doc_type.to_string(),
        doc_id: doc_id.to_string(),
        model: model.to_string(),
        dim: CountingEmbedding::DIMENSION as u32,
        vector: CountingEmbedding::vector(text),
        updated_at: now_ms(),
    };
    storage
        .upsert_memory_vector(&row)
        .await
        .expect("写入向量应成功");
}

/// 指纹匹配：已有向量直接装载，不触发任何嵌入调用，向量通道可命中。
#[tokio::test]
async fn build_loads_vectors_by_fingerprint_without_backfill() {
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-load", RamariaConfig::default()).await;
    seed_persona(&storage, "char-0001").await;
    let l1_id = seed_l1(&storage, "char-0001", "用户最近工作压力很大，常常加班").await;
    let event = MemoryEvent::new(
        "char-0001".to_string(),
        "工作压力".to_string(),
        "用户聊到工作压力".to_string(),
        1_000,
        2_000,
    );
    let event_id = storage.save_event(&event).await.expect("写入事件应成功");

    let fingerprint = CountingEmbedding::fingerprint();
    seed_vector(
        &storage,
        ramaria_core::types::VECTOR_DOC_L1,
        &l1_id.to_string(),
        &fingerprint,
        "用户最近工作压力很大，常常加班",
    )
    .await;
    seed_vector(
        &storage,
        ramaria_core::types::VECTOR_DOC_L2,
        &event_id.to_string(),
        &fingerprint,
        "工作压力",
    )
    .await;

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(embedding.batch_calls(), 0, "命中向量时不应批量嵌入");
    assert_eq!(embedding.embed_calls(), 0, "命中向量时不应单条嵌入");
    let view = engine.index_status();
    assert_eq!(view.status, IndexStatus::Ready);
    assert_eq!(view.loaded, 2);
    assert_eq!(view.total, 2);
    assert_eq!(view.percent, 100);
    assert!(view.last_error.is_none());

    let result = recall_l1(&engine, "工作压力").await;
    assert!(
        result.stats.channels.get("vector").copied().unwrap_or(0) > 0,
        "已装载向量应参与向量通道: {:?}",
        result.stats.channels
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 指纹不匹配：旧模型行不装载、不误用；当前缺失条目补算后按新指纹写入，旧行保留。
#[tokio::test]
async fn build_ignores_other_fingerprint_and_backfills_new_rows() {
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-mismatch", RamariaConfig::default()).await;
    seed_persona(&storage, "char-0001").await;
    let l1_id = seed_l1(&storage, "char-0001", "用户最近开始夜跑").await;

    let old_model = "old-embedding@16#64";
    seed_vector(
        &storage,
        ramaria_core::types::VECTOR_DOC_L1,
        &l1_id.to_string(),
        old_model,
        "旧模型向量",
    )
    .await;
    // 与当前语料无关的历史行：验证按模型过滤后仍然保留
    seed_vector(
        &storage,
        ramaria_core::types::VECTOR_DOC_L1,
        "stale-doc",
        old_model,
        "历史残留",
    )
    .await;

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(embedding.batch_calls(), 1, "缺失条目应走一次批量补算");
    let rows = storage
        .load_memory_vectors(&CountingEmbedding::fingerprint())
        .await
        .expect("加载新指纹向量应成功");
    assert_eq!(rows.len(), 1, "当前条目应按新指纹写入");
    assert_eq!(rows[0].doc_id, l1_id.to_string());
    let legacy = storage
        .load_memory_vectors(old_model)
        .await
        .expect("加载旧指纹向量应成功");
    assert_eq!(legacy.len(), 1, "无业务对应关系的旧指纹行应保留");
    assert_eq!(legacy[0].doc_id, "stale-doc");
    assert_eq!(engine.index_status().loaded, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 补算批大小：严格不超过 `[index].embed_batch_size`（5 条 / 每批 2 → [2,2,1]）。
#[tokio::test]
async fn backfill_respects_configured_batch_size() {
    let mut config = RamariaConfig::default();
    config.index.embed_batch_size = 2;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-batch", config).await;
    seed_persona(&storage, "char-0001").await;
    for index in 0..5 {
        seed_l1(&storage, "char-0001", &format!("补算条目 {index}")).await;
    }

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(embedding.batch_sizes(), vec![2, 2, 1]);
    assert_eq!(engine.index_status().loaded, 5);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 批失败降级：批量嵌入报错后逐条嵌入，全部条目最终写入。
#[tokio::test]
async fn backfill_falls_back_to_single_embed_when_batch_fails() {
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-batch-fail", RamariaConfig::default()).await;
    seed_persona(&storage, "char-0001").await;
    for index in 0..3 {
        seed_l1(&storage, "char-0001", &format!("降级条目 {index}")).await;
    }
    embedding.set_fail_batch(true);

    engine.rebuild_index().await.expect("批量失败也应完成构建");

    assert_eq!(embedding.batch_calls(), 1, "先尝试一次批量嵌入");
    assert_eq!(embedding.embed_calls(), 3, "批量失败后逐条补算");
    let rows = storage
        .load_memory_vectors(&CountingEmbedding::fingerprint())
        .await
        .expect("加载向量应成功");
    assert_eq!(rows.len(), 3, "逐条补算成功后应全部写入");
    assert_eq!(engine.index_status().loaded, 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 向量开关关闭：零嵌入调用、状态 Ready、total=0、percent=100。
#[tokio::test]
async fn disabled_vector_channel_skips_all_embedding() {
    let mut config = RamariaConfig::default();
    config.retrieval.enable_vector = false;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-disabled", config).await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "关闭向量通道的摘要").await;

    assert_eq!(
        engine.index_status().status,
        IndexStatus::Idle,
        "初始状态应为 idle"
    );

    engine.rebuild_index().await.expect("重建应成功");

    assert_eq!(embedding.batch_calls(), 0);
    assert_eq!(embedding.embed_calls(), 0);
    let view = engine.index_status();
    assert_eq!(view.status, IndexStatus::Ready);
    assert_eq!(view.loaded, 0);
    assert_eq!(view.total, 0);
    assert_eq!(view.percent, 100);
    let rows = storage
        .load_memory_vectors(&CountingEmbedding::fingerprint())
        .await
        .expect("加载向量应成功");
    assert!(rows.is_empty(), "关闭向量通道不应写入向量行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 增量镜像同样遵守向量开关：关闭时不调用嵌入。
#[tokio::test]
async fn disabled_vector_channel_skips_incremental_embedding() {
    let mut config = RamariaConfig::default();
    config.retrieval.enable_vector = false;
    let (engine, storage, embedding, dir) =
        engine_with_counting_embedding("index-vector-disabled-incremental", config).await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let mut l1 = ramaria_core::types::MemoryL1::new(session.id, "增量摘要".to_string(), None);
    l1.persona_uid = Some("char-0001".to_string());
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    super::index_l1_into_mirrors(&engine, &l1).await;

    assert_eq!(embedding.embed_calls(), 0, "关闭向量通道时增量不得嵌入");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 构建失败：状态切 degraded 并携带脱敏原因，旧索引保留。
#[tokio::test]
async fn build_failure_switches_status_to_degraded() {
    let (engine, storage, failable, dir) =
        engine_with_failable_storage("index-status-degraded").await;
    seed_persona(&storage, "char-0001").await;
    seed_l1(&storage, "char-0001", "失败前已可检索的摘要").await;

    engine.rebuild_index().await.expect("首次重建应成功");
    assert_eq!(engine.index_status().status, IndexStatus::Ready);

    failable.set_fail_list_personas(true);
    assert!(
        engine.rebuild_index().await.is_err(),
        "故障注入后重建应失败"
    );

    let view = engine.index_status();
    assert_eq!(view.status, IndexStatus::Degraded);
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|reason| !reason.is_empty()),
        "降级状态应携带失败原因"
    );
    assert!(engine.is_retriever_loaded(), "旧索引应保持可用");

    let _ = std::fs::remove_dir_all(&dir);
}
