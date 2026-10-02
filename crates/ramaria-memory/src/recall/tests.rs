//! crates/ramaria-memory/src/recall/tests.rs - //! crates/ramaria-memory/src/recall.rs - 召回装配共用实现单元测试
//!
//! 设计特点:
//! - 位于 recall 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 recall.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use crate::keyword::service::KeywordService;
use crate::retriever::{L1DocView, L2DocView, UttDocView};
use ramaria_core::config::RamariaConfig;
use std::sync::{Arc, RwLock};

/// 用真实 SQLite（临时文件库）构造存储后端：召回只用到 touch_l1，无需 mock 全 trait。
async fn test_storage(tag: &str) -> Arc<dyn StorageBackend> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-recall-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
        .await
        .expect("测试库初始化应成功");
    Arc::new(ramaria_storage::SqliteStorage::new(pool))
}

fn config() -> RamariaConfig {
    RamariaConfig::default()
}

fn seeded_retriever(summary: &str, persona_uid: &str) -> RwLock<Retriever> {
    let mut retriever = Retriever::new();
    retriever.index_l1(&L1DocView {
        id: Uuid::new_v4(),
        summary: summary.to_string(),
        keywords: Some("工作压力,加班".to_string()),
        persona_uid: Some(persona_uid.to_string()),
        created_at: 1_000,
        salience: 0.8,
        last_accessed_at: None,
    });
    RwLock::new(retriever)
}

/// 正常检索：命中 L1 → 上下文文本、覆盖集合、结构化条目齐备。
#[tokio::test]
async fn hit_assembles_context_and_hits() {
    let cfg = config();
    let storage = test_storage("hit").await;
    let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");

    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    let context = output.memory_context.expect("应组装记忆上下文");
    assert!(context.contains("工作压力"), "上下文应含摘要: {context}");
    assert_eq!(output.doc_labels.len(), 1, "覆盖集合应含命中 L1");
    assert_eq!(output.hits.len(), 1);
    assert_eq!(output.fused_count, 1);
    assert_eq!(output.filtered_count, 1);
}

/// 闸门全关：不检索、不生成向量，输出为空结构。
#[tokio::test]
async fn gates_closed_returns_empty() {
    let cfg = config();
    let storage = test_storage("gates").await;
    let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");

    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    assert!(output.memory_context.is_none());
    assert!(output.utt_context.is_none());
    assert!(output.hits.is_empty());
    assert_eq!(output.fused_count, 0);
}

/// 索引未加载（服务层懒加载槽为 None）：空召回且不报错。
#[tokio::test]
async fn unloaded_slot_returns_empty() {
    let cfg = config();
    let storage = test_storage("slot").await;
    let slot: RwLock<Option<Retriever>> = RwLock::new(None);

    let output = assemble_recall(RecallInput {
        retriever: &slot,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    assert!(output.memory_context.is_none());
    assert_eq!(output.fused_count, 0);
}

/// Persona-Aware 过滤：低 share 的 L2 事件对角色类 persona 不可见。
#[tokio::test]
async fn persona_filter_hides_low_share_event() {
    let cfg = config();
    let storage = test_storage("filter").await;
    let retriever = RwLock::new(Retriever::new());
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_l2(&L2DocView {
            id: 42,
            title: "低分享事件".to_string(),
            summary: "用户提到秘密".to_string(),
            keywords: Some("秘密".to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: "char-0001".to_string(),
            share: 0.1,
            confidence: 0.9,
            created_at: 1_000,
            salience: 0.8,
        });
    }

    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "秘密",
        persona_uid: Some("char-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    assert_eq!(output.fused_count, 1, "融合层应命中事件");
    assert_eq!(output.filtered_count, 0, "share 低于角色阈值应被过滤");
    assert!(output.memory_context.is_none());
}

/// 记忆子层开关：只请求 L1 时不返回 L2（反向亦然），段落文本同步收窄。
#[tokio::test]
async fn memory_layer_switch_filters_sublayers() {
    let cfg = config();
    let storage = test_storage("layers").await;
    let retriever = RwLock::new(Retriever::new());
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_l1(&L1DocView {
            id: Uuid::new_v4(),
            summary: "用户最近工作压力很大（摘要侧）".to_string(),
            keywords: Some("工作压力".to_string()),
            persona_uid: Some("char-0001".to_string()),
            created_at: 1_000,
            salience: 0.8,
            last_accessed_at: None,
        });
        guard.index_l2(&L2DocView {
            id: 7,
            title: "工作压力事件".to_string(),
            summary: "用户在群聊里被点名批评（事件侧）".to_string(),
            keywords: Some("工作压力".to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: "char-0001".to_string(),
            share: 1.0,
            confidence: 0.9,
            created_at: 1_000,
            salience: 0.8,
        });
    }

    let run = |layers: RecallMemoryLayers| {
        let retriever = &retriever;
        let storage = storage.as_ref();
        let cfg = &cfg;
        async move {
            assemble_recall(RecallInput {
                retriever,
                keyword_mirror: &RwLock::new(KeywordService::new()),
                storage,
                embedding: None,
                query: "工作压力",
                persona_uid: Some("char-0001"),
                retrieval: &cfg.retrieval,
                decay: &cfg.decay,
                utt: &cfg.utt,
                gates: RecallGates {
                    memory_rag: true,
                    utt: false,
                },
                memory_layers: layers,
                now_ms: 2_000,
            })
            .await
        }
    };

    // 先确认两层都能被命中（否则下面的断言没有意义）
    let both = run(RecallMemoryLayers::both()).await;
    assert!(
        both.hits.iter().any(|h| h.layer == "l1") && both.hits.iter().any(|h| h.layer == "l2"),
        "两层应都能命中: {:?}",
        both.hits
    );

    // 只要 L1：无 L2 条目、段落不含事件侧文本
    let l1_only = run(RecallMemoryLayers::l1_only()).await;
    assert!(
        l1_only.hits.iter().all(|h| h.layer == "l1"),
        "不应含 L2 条目"
    );
    let context = l1_only.memory_context.unwrap_or_default();
    assert!(context.contains("摘要侧"), "应含 L1 文本: {context}");
    assert!(!context.contains("事件侧"), "不应含 L2 文本: {context}");

    // 只要 L2：无 L1 条目、段落不含摘要侧文本
    let l2_only = run(RecallMemoryLayers::l2_only()).await;
    assert!(
        l2_only.hits.iter().all(|h| h.layer == "l2"),
        "不应含 L1 条目"
    );
    let context = l2_only.memory_context.unwrap_or_default();
    assert!(context.contains("事件侧"), "应含 L2 文本: {context}");
    assert!(!context.contains("摘要侧"), "不应含 L1 文本: {context}");

    // 开关判定：图谱等非摘要层不受约束
    assert!(RecallMemoryLayers::l2_only().allows("graph"));
    assert!(RecallMemoryLayers::l1_only().any());
    assert!(!RecallMemoryLayers::l2_only().allows("l1"));
}

/// utt 原文通道：白名单内 persona + 闸门开启 → 渲染原文片段；白名单外不注入。
#[tokio::test]
async fn utt_respects_persona_whitelist() {
    let cfg = config();
    let storage = test_storage("utt").await;
    let retriever = RwLock::new(Retriever::new());
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_utt(
            &UttDocView {
                id: 1,
                persona_uid: "char-0001".to_string(),
                session_id: Uuid::new_v4(),
                block_text: "今天天气真好我们一起去公园".to_string(),
                msg_count: 2,
                created_at: 1_000,
            },
            None,
        );
    }

    // 白名单内（char-0001）→ 注入
    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "公园",
        persona_uid: Some("char-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: true,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;
    assert!(output.utt_context.is_some(), "白名单内应注入原文片段");

    // 白名单外（rama-0001）→ 不注入
    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "公园",
        persona_uid: Some("rama-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: true,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;
    assert!(output.utt_context.is_none(), "白名单外不注入原文");
}

/// 关键词镜像通道：镜像为空时静默跳过（不影响 BM25 命中）。
#[tokio::test]
async fn keyword_mirror_empty_degrades_silently() {
    let cfg = config();
    let storage = test_storage("mirror").await;
    let retriever = seeded_retriever("用户讨论Rust编程", "persona-0001");

    let output = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "Rust",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    assert!(output.memory_context.is_some(), "BM25 通道应命中");
    assert_eq!(output.channels.keyword, 0, "镜像为空时关键词通道命中数为 0");
}

/// 对照测试：同一输入经两种句柄（在线管线的 `RwLock<Retriever>` 与服务层的
/// `RwLock<Option<Retriever>>` 懒加载槽）产出完全一致 —— 召回同源的直接证据。
#[tokio::test]
async fn both_handles_produce_identical_output() {
    let cfg = config();
    let storage = test_storage("both").await;

    // 两份内容相同的索引（文档 id 不同，不参与文本/分数比对）
    let make_doc = || L1DocView {
        id: Uuid::new_v4(),
        summary: "用户最近工作压力很大".to_string(),
        keywords: Some("工作压力,加班".to_string()),
        persona_uid: Some("persona-0001".to_string()),
        created_at: 1_000,
        salience: 0.8,
        last_accessed_at: None,
    };
    let mut plain_retriever = Retriever::new();
    plain_retriever.index_l1(&make_doc());
    let plain = RwLock::new(plain_retriever);

    let mut slot_retriever = Retriever::new();
    slot_retriever.index_l1(&make_doc());
    let slot: RwLock<Option<Retriever>> = RwLock::new(Some(slot_retriever));

    let out_plain = assemble_recall(RecallInput {
        retriever: &plain,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    let out_slot = assemble_recall(RecallInput {
        retriever: &slot,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
    })
    .await;

    assert_eq!(
        out_plain.memory_context, out_slot.memory_context,
        "两种句柄的上下文文本必须一致"
    );
    assert_eq!(out_plain.channels, out_slot.channels);
    assert_eq!(out_plain.fused_count, out_slot.fused_count);
    assert_eq!(out_plain.filtered_count, out_slot.filtered_count);
    let texts = |out: &RecallOutput| -> Vec<(String, String)> {
        out.hits
            .iter()
            .map(|h| (h.layer.clone(), format!("{:.6}", h.score)))
            .collect()
    };
    assert_eq!(texts(&out_plain), texts(&out_slot));
}

/// 通道命中计数映射：四个通道都出现在 `as_map` 输出中（含 0 值）。
#[test]
fn channels_map_contains_all_channels() {
    let map = RecallChannels {
        vector: 2,
        bm25: 3,
        keyword: 1,
        graph: 0,
    }
    .as_map();
    assert_eq!(map.get("vector"), Some(&2));
    assert_eq!(map.get("bm25"), Some(&3));
    assert_eq!(map.get("keyword"), Some(&1));
    assert_eq!(map.get("graph"), Some(&0));
}
