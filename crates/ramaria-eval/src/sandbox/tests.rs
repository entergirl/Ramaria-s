//! crates/ramaria-eval/src/sandbox/tests.rs - Ramaria 评测沙盒单元测试
//!
//! 设计特点:
//! - 解析断言覆盖角色别名、多行拼接、空行分轮与带行号的错误
//! - 目录准备断言旧内容被整体清空
//! - 默认人格与写入走真实评测装配，验证消息条数与人格文件落点
//! - 轨迹渲染用合成轨迹覆盖四通道、融合、两类丢弃、预算与原文通道
//! - 每个用例独立临时目录，结束前关闭连接池再删除

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_service::{
    BudgetTrace, ChannelCandidate, ChannelTrace, ChatRole, ChatTurn, DropReason, DroppedTrace,
    Engine, FusedEntryTrace, IngestRequest, RecallTrace, RetrievalTrace, TraceChannel, UttTrace,
};

use super::pipeline::{
    DEFAULT_SANDBOX_PERSONA_NAME, DEFAULT_SANDBOX_PERSONA_UID, ensure_default_persona,
};
use super::*;

// =========================================================
// 夹具
// =========================================================

/// 独立临时沙盒根：同一进程内并行用例互不干扰。
fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-sandbox-{tag}-{nanos}"))
}

/// 删除临时目录；清理失败不掩盖断言结果。
fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// 关闭引擎持有的连接池，释放 SQLite 文件句柄。
async fn close(engine: Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
}

/// 构造一条对话片段。
fn turn(role: ChatRole, content: &str) -> ChatTurn {
    ChatTurn {
        role,
        content: content.to_string(),
    }
}

// =========================================================
// 对话块解析
// =========================================================

/// 英文角色前缀大小写不敏感。
#[test]
fn parse_dialogue_accepts_english_prefixes_in_any_case() {
    let turns = parse_dialogue(
        "User: 你好
ASSISTANT: 在的
",
    )
    .expect("解析应成功");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, ChatRole::User);
    assert_eq!(turns[0].content, "你好");
    assert_eq!(turns[1].role, ChatRole::Assistant);
    assert_eq!(turns[1].content, "在的");
}

/// 中文别名（用户 / 助手）与英文前缀等价。
#[test]
fn parse_dialogue_accepts_chinese_aliases() {
    let turns = parse_dialogue(
        "用户: 早
助手: 早呀",
    )
    .expect("解析应成功");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, ChatRole::User);
    assert_eq!(turns[0].content, "早");
    assert_eq!(turns[1].role, ChatRole::Assistant);
    assert_eq!(turns[1].content, "早呀");
}

/// 轮内无前缀行按续行拼接，空行结束当前轮。
#[test]
fn parse_dialogue_joins_continuation_lines_and_blank_separates_turns() {
    let turns = parse_dialogue(
        "user: 第一行
第二行

assistant: 收到
第三行",
    )
    .expect("解析应成功");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].content, "第一行\n第二行");
    assert_eq!(turns[1].content, "收到\n第三行");
}

/// 轮外无前缀行报错并带行号。
#[test]
fn parse_dialogue_rejects_prefixless_line_with_line_number() {
    let error = parse_dialogue(
        "user: 你好

没有前缀",
    )
    .expect_err("轮外缺角色前缀应报错");
    assert!(error.contains("第 3 行"), "错误应含行号，实际: {error}");
}

// =========================================================
// 行模式输入解析
// =========================================================

/// @persona 首行、对话与 --- 之后的问题都能解析；未给 @backend 时继承当前配置。
#[test]
fn parse_line_input_reads_persona_dialogue_and_question() {
    let text = "@persona cards/hero.toml
user: 我叫小林
assistant: 记住了
---

你记得我叫什么吗？
";
    let parsed = parse_line_input(text).expect("解析应成功");
    assert_eq!(parsed.backend, SandboxBackendChoice::Inherit);
    assert!(!parsed.backend_explicit, "未写后端指令时不算显式指定");
    assert_eq!(
        parsed.input.persona_path,
        Some(PathBuf::from("cards/hero.toml"))
    );
    assert_eq!(parsed.input.turns.len(), 2);
    assert_eq!(parsed.input.turns[0].content, "我叫小林");
    assert_eq!(parsed.input.question, "你记得我叫什么吗？");
}

/// @backend local 与地址 / 模型 / 向量指令组合解析为独立配置。
#[test]
fn parse_line_input_reads_independent_backend_directives() {
    let text = "@backend local
@base-url http://localhost:9999/v1
@model qwen-local
@embedding models/bge-small
user: 你好
---
还记得我吗
";
    let parsed = parse_line_input(text).expect("解析应成功");
    assert!(parsed.backend_explicit, "写了 @backend 即为显式指定");
    match parsed.backend {
        SandboxBackendChoice::Independent {
            kind,
            base_url,
            model_id,
            embedding_model_path,
        } => {
            assert_eq!(kind, SandboxModelKind::Local);
            assert_eq!(base_url, "http://localhost:9999/v1");
            assert_eq!(model_id, "qwen-local");
            assert_eq!(
                embedding_model_path,
                Some(PathBuf::from("models/bge-small"))
            );
        }
        other => panic!("应解析为独立配置，实际: {other:?}"),
    }
    assert_eq!(parsed.input.turns.len(), 1);
    assert_eq!(parsed.input.question, "还记得我吗");
}

/// 后端指令与人格指令分开标记：@persona 不改变来源显式性。
#[test]
fn parse_line_input_marks_explicit_backend() {
    let implicit =
        parse_line_input("@persona cards/hero.toml\nuser: 你好\n---\n问题\n").expect("解析应成功");
    assert!(!implicit.backend_explicit);
    assert_eq!(implicit.backend, SandboxBackendChoice::Inherit);

    let explicit = parse_line_input(
        "@backend cloud\n@base-url https://host/v1\n@model m\nuser: 你好\n---\n问题\n",
    )
    .expect("解析应成功");
    assert!(explicit.backend_explicit);
}

/// 继承模式（显式或默认）不接受独立配置项。
#[test]
fn parse_line_input_rejects_independent_directives_in_inherit_mode() {
    for text in [
        "@backend inherit
@model qwen-local
user: 你好
---
问题
",
        "@base-url http://localhost:9999/v1
user: 你好
---
问题
",
    ] {
        let error = parse_line_input(text).expect_err("继承模式带独立配置项应报错");
        assert!(
            error.contains("继承模式不接受独立配置"),
            "错误应说明互斥关系，实际: {error}"
        );
    }
}

/// 同一指令重复出现报错并带行号。
#[test]
fn parse_line_input_rejects_duplicate_backend_directive() {
    let error = parse_line_input(
        "@backend local
@backend cloud
user: 你好
---
问题
",
    )
    .expect_err("重复指令应报错");
    assert!(error.contains("第 2 行"), "错误应含行号，实际: {error}");
    assert!(error.contains("重复"), "错误应说明重复，实际: {error}");
}

/// @backend 缺参数与未知取值都报错并附格式提示。
#[test]
fn parse_line_input_rejects_invalid_backend_directive() {
    let missing = parse_line_input(
        "@backend
user: 你好
---
问题
",
    )
    .expect_err("缺参数应报错");
    assert!(
        missing.contains("第 1 行 @backend 缺少参数"),
        "错误应说明缺参数，实际: {missing}"
    );

    let unknown = parse_line_input(
        "@backend remote
user: 你好
---
问题
",
    )
    .expect_err("未知取值应报错");
    assert!(
        unknown.contains("只接受 inherit / local / cloud"),
        "错误应说明取值域，实际: {unknown}"
    );
}

/// 缺少 --- 分隔线时报错并给出格式提示。
#[test]
fn parse_line_input_requires_separator() {
    let error = parse_line_input(
        "user: 你好
你记得我吗",
    )
    .expect_err("缺分隔线应报错");
    assert!(error.contains("---"), "错误应提示分隔线，实际: {error}");
}

/// 问题为空时报错。
#[test]
fn parse_line_input_rejects_empty_question() {
    let error = parse_line_input(
        "user: 你好
---
   
",
    )
    .expect_err("空问题应报错");
    assert!(
        error.contains("问题为空"),
        "错误应说明问题为空，实际: {error}"
    );
}

// =========================================================
// 目录准备
// =========================================================

/// 已存在的目录被清空后重建。
#[test]
fn prepare_clears_existing_directory() {
    let root = temp_root("prepare");
    std::fs::create_dir_all(&root).expect("预建目录应成功");
    std::fs::write(root.join("stale.txt"), "old").expect("预置文件应成功");

    prepare(&root).expect("目录准备应成功");

    assert!(root.is_dir(), "准备后应为目录");
    let entries = std::fs::read_dir(&root).expect("目录应可读").count();
    assert_eq!(entries, 0, "旧内容应被清空");
    cleanup(&root);
}

// =========================================================
// 默认人格与写入
// =========================================================

/// 默认人格落在沙盒根下，写入 8 条消息后可读回同等条数。
#[tokio::test]
async fn default_persona_and_ingest_write_all_turns() {
    let root = temp_root("ingest");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");

    let uid = ensure_default_persona(&engine, &root)
        .await
        .expect("默认人格应创建");
    assert_eq!(uid, DEFAULT_SANDBOX_PERSONA_UID);

    let path = root
        .join("persona")
        .join(format!("{DEFAULT_SANDBOX_PERSONA_UID}.toml"));
    let content = std::fs::read_to_string(&path).expect("人格文件应存在");
    assert!(
        content.contains(DEFAULT_SANDBOX_PERSONA_NAME),
        "人格文件应含名称"
    );

    let turns = vec![
        turn(ChatRole::User, "你好"),
        turn(ChatRole::Assistant, "你好呀"),
        turn(ChatRole::User, "今天加班到很晚"),
        turn(ChatRole::Assistant, "辛苦了，早点休息"),
        turn(ChatRole::User, "周末想去爬山"),
        turn(ChatRole::Assistant, "记得带水和外套"),
        turn(ChatRole::User, "你喜欢什么季节"),
        turn(ChatRole::Assistant, "秋天"),
    ];
    let outcome = engine
        .ingest(IngestRequest {
            messages: turns,
            persona: Some(uid),
            conversation_id: Some("sandbox".to_string()),
            channel: "sandbox".to_string(),
            finalize: false,
        })
        .await
        .expect("写入应成功");

    assert_eq!(outcome.written, 8, "8 条消息都应写入");
    assert_eq!(
        engine.count_session_messages(outcome.session_id).await,
        8,
        "会话内消息数应与写入一致"
    );

    close(engine).await;
    cleanup(&root);
}

// =========================================================
// 轨迹渲染
// =========================================================

/// 四通道、融合、两类丢弃、预算与原文通道都出现在渲染文本中。
#[test]
fn trace_lines_cover_channels_fusion_drops_budget_and_utt() {
    let trace = RecallTrace {
        query_chars: 12,
        query_vector_dim: Some(768),
        retrieval: RetrievalTrace {
            channels: vec![
                ChannelTrace {
                    channel: TraceChannel::Vector,
                    active: true,
                    candidates: vec![ChannelCandidate {
                        id: "L1:a".to_string(),
                        raw_score: 0.9,
                        rank: 1,
                    }],
                },
                ChannelTrace {
                    channel: TraceChannel::Bm25,
                    active: true,
                    candidates: vec![ChannelCandidate {
                        id: "L2:7".to_string(),
                        raw_score: 1.25,
                        rank: 1,
                    }],
                },
                ChannelTrace {
                    channel: TraceChannel::Graph,
                    active: false,
                    candidates: Vec::new(),
                },
                ChannelTrace {
                    channel: TraceChannel::Keyword,
                    active: false,
                    candidates: Vec::new(),
                },
            ],
            fused: vec![FusedEntryTrace {
                id: "L2:7".to_string(),
                layer: "l2".to_string(),
                rrf_score: 0.03,
                decay_factor: 0.87,
                decayed_score: 0.026,
                vector_raw: Some(0.9),
                bm25_raw: Some(1.25),
                graph_raw: None,
                keyword_raw: Some(0.4),
            }],
            dropped: vec![DroppedTrace {
                id: "L2:8".to_string(),
                reason: DropReason::FusionCut,
                score: 0.5,
            }],
        },
        dropped: vec![DroppedTrace {
            id: "L2:9".to_string(),
            reason: DropReason::MaxMemories,
            score: 0.1,
        }],
        budget: BudgetTrace {
            max_memories: 5,
            candidates: 6,
            kept: 5,
            tokens_before: 120,
            tokens_after: 96,
        },
        utt: UttTrace {
            active: true,
            candidates: vec![ChannelCandidate {
                id: "L0:1".to_string(),
                raw_score: 0.81,
                rank: 1,
            }],
            rendered_chars: 240,
            budget_chars: 1500,
        },
    };

    let text = trace_lines(&trace).join("\n");
    for needle in [
        "12 字符",
        "768",
        "通道 vector",
        "通道 bm25",
        "通道 graph",
        "通道 keyword",
        "未参与",
        "L2:7",
        "rrf=",
        "decay=",
        "decayed=",
        "fusion_cut",
        "max_memories",
        "上限 5",
        "候选 6",
        "保留 5",
        "120",
        "96",
        "L0:1",
        "240",
        "1500",
    ] {
        assert!(text.contains(needle), "轨迹应包含 {needle}，实际:\n{text}");
    }
}
