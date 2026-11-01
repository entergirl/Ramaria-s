//! crates/ramaria-memory/src/recall/tests.rs - //! crates/ramaria-memory/src/recall.rs - 召回装配共用实现单元测试
//!
//! 设计特点:
//! - 位于 recall 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 recall.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use crate::keyword::service::KeywordService;
use crate::retriever::{L1DocView, L2DocView, Retriever, UttDocView};
use crate::trace::{DropReason, RecallTrace, TraceChannel};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryL1, Persona, PersonaKind};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// 用真实 SQLite（临时文件库）构造存储后端：召回只用到 touch_l1，无需 mock 全 trait。
async fn test_storage(tag: &str) -> Arc<dyn StorageBackend> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-recall-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    let pool = ramaria_storage::database::init_pool(Some(dir.join("ramaria.db")))
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
        occurred_at: 1_000,
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
            start: 1_000,
            end: 1_000,
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
            occurred_at: 1_000,
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
            start: 1_000,
            end: 1_000,
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
                persona_kind: Some(PersonaKind::Char),
                retrieval: &cfg.retrieval,
                decay: &cfg.decay,
                utt: &cfg.utt,
                gates: RecallGates {
                    memory_rag: true,
                    utt: false,
                },
                memory_layers: layers,
                now_ms: 2_000,
                as_of_ms: None,
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

    // 开关判定：图谱实体来源为 L2 事件，跟随 l2 开关（仅 L1 时不参与）
    assert!(RecallMemoryLayers::l2_only().allows("graph"));
    assert!(!RecallMemoryLayers::l1_only().allows("graph"));
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: true,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
        persona_kind: Some(PersonaKind::Rama),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: false,
            utt: true,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
    })
    .await;
    assert!(output.utt_context.is_none(), "白名单外不注入原文");
}

/// 显式载体（uid 与 kind 背离）：utt 白名单按 `persona_kind` 判定，不看 uid 前缀。
#[tokio::test]
async fn utt_whitelist_follows_persona_kind_not_uid_prefix() {
    let cfg = config();
    let storage = test_storage("utt-drift").await;
    let retriever = RwLock::new(Retriever::new());
    let make_utt = |id: i64, persona_uid: &str| UttDocView {
        id,
        persona_uid: persona_uid.to_string(),
        session_id: Uuid::new_v4(),
        block_text: "今天天气真好我们一起去公园".to_string(),
        msg_count: 2,
        created_at: 1_000,
    };
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_utt(&make_utt(1, "user-9999"), None);
        guard.index_utt(&make_utt(2, "char-9999"), None);
    }

    let run = |persona_uid: &'static str, kind: PersonaKind| {
        let retriever = &retriever;
        let storage = storage.as_ref();
        let cfg = &cfg;
        async move {
            assemble_recall(RecallInput {
                retriever,
                keyword_mirror: &RwLock::new(KeywordService::new()),
                storage,
                embedding: None,
                query: "公园",
                persona_uid: Some(persona_uid),
                persona_kind: Some(kind),
                retrieval: &cfg.retrieval,
                decay: &cfg.decay,
                utt: &cfg.utt,
                gates: RecallGates {
                    memory_rag: false,
                    utt: true,
                },
                memory_layers: RecallMemoryLayers::default(),
                now_ms: 2_000,
                as_of_ms: None,
            })
            .await
        }
    };

    // uid 前缀形似 user，但类型为角色类 → 放行（旧前缀推导会拒绝）
    let out = run("user-9999", PersonaKind::Char).await;
    assert!(
        out.utt_context.is_some(),
        "类型为角色类应放行原文注入（不按 uid 前缀拒绝）"
    );

    // uid 前缀形似 char，但类型为 user 类 → 拒绝（旧前缀推导会放行）
    let out = run("char-9999", PersonaKind::User).await;
    assert!(
        out.utt_context.is_none(),
        "类型为 user 类应拒绝原文注入（不按 uid 前缀放行）"
    );
}

/// Persona-Aware 阈值按显式类型：share=0.4 对 User 保留、对 Char 过滤、未知（None）按最严。
#[tokio::test]
async fn rag_threshold_follows_persona_kind() {
    let cfg = config();
    let storage = test_storage("kind-threshold").await;
    let retriever = RwLock::new(Retriever::new());
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_l2(&L2DocView {
            id: 7,
            title: "边界分享事件".to_string(),
            summary: "用户提到边界内容".to_string(),
            keywords: Some("边界".to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: "char-0001".to_string(),
            share: 0.4,
            confidence: 0.9,
            created_at: 1_000,
            start: 1_000,
            end: 1_000,
            salience: 0.8,
        });
    }

    let run = |kind: Option<PersonaKind>| {
        let retriever = &retriever;
        let storage = storage.as_ref();
        let cfg = &cfg;
        async move {
            assemble_recall(RecallInput {
                retriever,
                keyword_mirror: &RwLock::new(KeywordService::new()),
                storage,
                embedding: None,
                query: "边界",
                persona_uid: Some("char-0001"),
                persona_kind: kind,
                retrieval: &cfg.retrieval,
                decay: &cfg.decay,
                utt: &cfg.utt,
                gates: RecallGates {
                    memory_rag: true,
                    utt: false,
                },
                memory_layers: RecallMemoryLayers::default(),
                now_ms: 2_000,
                as_of_ms: None,
            })
            .await
        }
    };

    let user = run(Some(PersonaKind::User)).await;
    assert_eq!(user.fused_count, 1, "融合层应命中事件");
    assert_eq!(
        user.filtered_count, 1,
        "share=0.4 达 user 阈值（0.3）应保留"
    );

    let char_kind = run(Some(PersonaKind::Char)).await;
    assert_eq!(
        char_kind.filtered_count, 0,
        "share=0.4 低于 char 阈值（0.5）应过滤"
    );

    let unknown = run(None).await;
    assert_eq!(unknown.filtered_count, 0, "类型未知按最严阈值过滤");
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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
        occurred_at: 1_000,
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
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
    })
    .await;

    let out_slot = assemble_recall(RecallInput {
        retriever: &slot,
        keyword_mirror: &RwLock::new(KeywordService::new()),
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
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

// =========================================================
// 召回轨迹
// =========================================================

/// 构造两篇摘要都命中「工作压力」的 L1 文档：用于条数预算与轨迹内容断言。
fn seeded_retriever_two_hits(persona_uid: &str) -> RwLock<Retriever> {
    let mut retriever = Retriever::new();
    for (offset, summary) in ["用户最近工作压力很大", "用户最近工作压力也来自加班"]
        .into_iter()
        .enumerate()
    {
        let at = 1_000 + offset as i64;
        retriever.index_l1(&L1DocView {
            id: Uuid::new_v4(),
            summary: summary.to_string(),
            keywords: Some("工作压力,加班".to_string()),
            persona_uid: Some(persona_uid.to_string()),
            created_at: at,
            occurred_at: at,
            salience: 0.8,
            last_accessed_at: None,
        });
    }
    RwLock::new(retriever)
}

/// 开 / 关轨迹：同一检索器与时间基准下，召回输出逐字段相等（只多出轨迹）。
#[tokio::test]
async fn trace_keeps_output_identical() {
    let cfg = config();
    let storage = test_storage("trace-same").await;
    let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");
    let mirror = RwLock::new(KeywordService::new());

    let plain = assemble_recall(RecallInput {
        retriever: &retriever,
        keyword_mirror: &mirror,
        storage: storage.as_ref(),
        embedding: None,
        query: "工作压力",
        persona_uid: Some("persona-0001"),
        persona_kind: Some(PersonaKind::Char),
        retrieval: &cfg.retrieval,
        decay: &cfg.decay,
        utt: &cfg.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: false,
        },
        memory_layers: RecallMemoryLayers::default(),
        now_ms: 2_000,
        as_of_ms: None,
    })
    .await;

    let mut trace = RecallTrace::default();
    let traced = assemble_recall_with(
        RecallInput {
            retriever: &retriever,
            keyword_mirror: &mirror,
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            persona_kind: Some(PersonaKind::Char),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
            as_of_ms: None,
        },
        RecallOptions {
            trace: Some(&mut trace),
            touch: true,
        },
    )
    .await;

    assert_eq!(traced.memory_context, plain.memory_context);
    assert_eq!(traced.doc_labels, plain.doc_labels);
    assert_eq!(traced.utt_context, plain.utt_context);
    assert_eq!(traced.hits, plain.hits);
    assert_eq!(traced.channels, plain.channels);
    assert_eq!(traced.fused_count, plain.fused_count);
    assert_eq!(traced.filtered_count, plain.filtered_count);

    assert_eq!(trace.query_chars, 4, "只记查询字符数，不记文本");
    assert!(!trace.retrieval.fused.is_empty(), "轨迹应含融合条目");
    assert!(!trace.utt.active, "utt 闸门关闭时通道未生效");
    assert!(
        trace
            .retrieval
            .dropped
            .iter()
            .all(|d| d.reason != DropReason::Future),
        "查询时钟缺省时不应产生未来剔除"
    );
}

/// 轨迹内容：通道候选、衰减回填、条数预算丢弃与预算计数。
#[tokio::test]
async fn trace_records_channels_decay_and_budget() {
    let mut cfg = config();
    cfg.retrieval.rag_max_memories = 1;
    let storage = test_storage("trace-content").await;
    let retriever = seeded_retriever_two_hits("persona-0001");
    let mirror = RwLock::new(KeywordService::new());

    let mut trace = RecallTrace::default();
    let output = assemble_recall_with(
        RecallInput {
            retriever: &retriever,
            keyword_mirror: &mirror,
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            persona_kind: Some(PersonaKind::Char),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
            as_of_ms: None,
        },
        RecallOptions {
            trace: Some(&mut trace),
            touch: true,
        },
    )
    .await;

    assert_eq!(output.fused_count, 2, "两篇摘要都应进入融合");
    let bm25 = trace
        .retrieval
        .channels
        .iter()
        .find(|c| c.channel == TraceChannel::Bm25)
        .expect("应有 BM25 通道轨迹");
    assert!(bm25.active, "BM25 通道应参与融合");
    assert_eq!(bm25.candidates.len(), 2);
    assert_eq!(bm25.candidates[0].rank, 1, "名次从 1 起");

    assert_eq!(trace.retrieval.fused.len(), 2);
    for entry in &trace.retrieval.fused {
        assert_eq!(entry.layer, "l1");
        assert!(
            entry.decayed_score <= entry.rrf_score,
            "衰减不应提高分数: {entry:?}"
        );
    }

    assert!(
        trace
            .dropped
            .iter()
            .any(|d| d.reason == DropReason::MaxMemories),
        "超出条数预算的条目应记入丢弃清单"
    );
    assert_eq!(trace.budget.candidates, 2);
    assert_eq!(trace.budget.kept, 1);
}

// =========================================================
// 查询时钟（as_of）
// =========================================================

/// 一天毫秒数（查询时钟用例的时间刻度）。
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// 未来过滤：事件结束时间晚于查询时钟时，该事件在融合前被剔除并记入轨迹。
#[tokio::test]
async fn future_units_dropped_before_fusion() {
    let cfg = config();
    let storage = test_storage("as-of-future").await;
    let retriever = RwLock::new(Retriever::new());
    let l1_id = Uuid::new_v4();
    {
        let mut guard = retriever.write().expect("锁可用");
        guard.index_l1(&L1DocView {
            id: l1_id,
            summary: "用户最近工作压力很大（历史摘要）".to_string(),
            keywords: Some("工作压力".to_string()),
            persona_uid: Some("char-0001".to_string()),
            created_at: 2_000,
            occurred_at: 2_000,
            salience: 0.8,
            last_accessed_at: None,
        });
        // 事件开始早于时钟、结束晚于时钟：只有按结束时间判定才会被剔除。
        guard.index_l2(&L2DocView {
            id: 9,
            title: "工作压力事件".to_string(),
            summary: "用户后来又一次提到工作压力".to_string(),
            keywords: Some("工作压力".to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: "char-0001".to_string(),
            share: 1.0,
            confidence: 0.9,
            created_at: 2_500,
            start: 2_500,
            end: 9_000,
            salience: 0.8,
        });
    }

    let mut trace = RecallTrace::default();
    let output = assemble_recall_with(
        RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("char-0001"),
            persona_kind: Some(PersonaKind::Char),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 10_000,
            as_of_ms: Some(5_000),
        },
        RecallOptions {
            trace: Some(&mut trace),
            touch: true,
        },
    )
    .await;

    assert_eq!(
        output.hits.len(),
        1,
        "只有历史摘要应命中: {:?}",
        output.hits
    );
    assert_eq!(output.hits[0].id, format!("L1:{l1_id}"));
    let context = output.memory_context.unwrap_or_default();
    assert!(
        context.contains("历史摘要"),
        "上下文应含历史摘要: {context}"
    );
    assert!(
        !context.contains("后来"),
        "未来事件不应进入上下文: {context}"
    );

    let future = trace
        .retrieval
        .dropped
        .iter()
        .find(|d| d.reason == DropReason::Future)
        .expect("应记录未来剔除");
    assert_eq!(future.id, "L2:9");
    assert!(future.score > 0.0, "分数应取该 label 的通道原始分");
}

/// 查询时钟即衰减基准：同一记忆在两个时钟节点上的衰减后分数不同，节点越晚分数越低。
#[tokio::test]
async fn query_clock_sets_decay_benchmark() {
    let cfg = config();
    let storage = test_storage("as-of-decay").await;
    let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");
    // 当前时间固定：若衰减误用 now_ms，两个时钟节点的分数会完全相同。
    let fixed_now = 1_000 + 10 * DAY_MS;

    let run = |as_of: i64| {
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
                persona_uid: Some("persona-0001"),
                persona_kind: Some(PersonaKind::Char),
                retrieval: &cfg.retrieval,
                decay: &cfg.decay,
                utt: &cfg.utt,
                gates: RecallGates {
                    memory_rag: true,
                    utt: false,
                },
                memory_layers: RecallMemoryLayers::default(),
                now_ms: fixed_now,
                as_of_ms: Some(as_of),
            })
            .await
        }
    };

    let early = run(1_000 + 10 * DAY_MS).await;
    let late = run(1_000 + 40 * DAY_MS).await;
    let score = |output: &RecallOutput| {
        output
            .hits
            .first()
            .expect("同一记忆在前后两个时钟都应命中")
            .score
    };
    assert!(
        score(&late) < score(&early),
        "更晚的查询时钟应得到更低的衰减后分数: early={}, late={}",
        score(&early),
        score(&late)
    );
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

// =========================================================
// 访问时间刷新开关（touch）
// =========================================================

/// 造一个 persona 行（L1 外键依赖）。
async fn seed_persona_row(storage: &dyn StorageBackend, persona_uid: &str) {
    let persona = Persona::new(
        persona_uid.to_string(),
        "测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona 应成功");
}

/// 读取 L1 行的访问时间（None = 从未被访问）。
async fn l1_access_time(storage: &dyn StorageBackend, id: Uuid) -> Option<i64> {
    storage
        .get_memory_l1(id)
        .await
        .expect("读取 L1 应成功")
        .expect("L1 行应存在")
        .last_accessed_at
}

/// touch=false：不写访问时间，且召回输出与 touch=true 逐字段相等。
#[tokio::test]
async fn touch_disabled_skips_access_time_write() {
    let cfg = config();
    let storage = test_storage("touch-switch").await;
    seed_persona_row(storage.as_ref(), "persona-0001").await;

    let session = storage
        .create_session(Some("persona-0001"))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(session.id, "用户最近工作压力很大".to_string(), None);
    l1.persona_uid = Some("persona-0001".to_string());
    l1.created_at = 1_000;
    let l1_id = l1.id;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    let retriever = RwLock::new({
        let mut retriever = Retriever::new();
        retriever.index_l1(&L1DocView {
            id: l1_id,
            summary: "用户最近工作压力很大".to_string(),
            keywords: Some("工作压力".to_string()),
            persona_uid: Some("persona-0001".to_string()),
            created_at: 1_000,
            occurred_at: 1_000,
            salience: 0.8,
            last_accessed_at: None,
        });
        retriever
    });

    let run = |touch: bool| {
        let retriever = &retriever;
        let storage = storage.as_ref();
        let cfg = &cfg;
        async move {
            assemble_recall_with(
                RecallInput {
                    retriever,
                    keyword_mirror: &RwLock::new(KeywordService::new()),
                    storage,
                    embedding: None,
                    query: "工作压力",
                    persona_uid: Some("persona-0001"),
                    persona_kind: Some(PersonaKind::Char),
                    retrieval: &cfg.retrieval,
                    decay: &cfg.decay,
                    utt: &cfg.utt,
                    gates: RecallGates {
                        memory_rag: true,
                        utt: false,
                    },
                    memory_layers: RecallMemoryLayers::default(),
                    now_ms: 2_000,
                    as_of_ms: None,
                },
                RecallOptions { trace: None, touch },
            )
            .await
        }
    };

    assert_eq!(
        l1_access_time(storage.as_ref(), l1_id).await,
        None,
        "新建 L1 不应有访问时间"
    );

    let untouched = run(false).await;
    assert_eq!(
        l1_access_time(storage.as_ref(), l1_id).await,
        None,
        "touch=false 不应刷新访问时间"
    );

    let touched = run(true).await;
    assert_eq!(
        l1_access_time(storage.as_ref(), l1_id).await,
        Some(2_000),
        "touch=true 应按查询时钟写入访问时间"
    );

    assert_eq!(untouched.memory_context, touched.memory_context);
    assert_eq!(untouched.doc_labels, touched.doc_labels);
    assert_eq!(untouched.utt_context, touched.utt_context);
    assert_eq!(untouched.hits, touched.hits);
    assert_eq!(untouched.channels, touched.channels);
    assert_eq!(untouched.fused_count, touched.fused_count);
    assert_eq!(untouched.filtered_count, touched.filtered_count);
}
