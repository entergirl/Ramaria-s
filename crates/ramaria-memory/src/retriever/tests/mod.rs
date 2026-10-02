//! crates/ramaria-memory/src/retriever/tests/mod.rs - 三通道组合检索编排器单元测试
//!
//! 设计特点:
//! - 覆盖 BM25/向量/图谱三通道索引、RRF 融合、persona 过滤与 doc_id 解析。
//! - 使用内存 Retriever（构造 L1/L2/L0 文档）做确定性断言，不依赖真实 LLM/embedding。
use super::*;

fn make_test_retriever() -> Retriever {
    let mut r = Retriever::new();

    // 添加 L1 文档
    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
        keywords: Some("学习,Rust,编程".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: 1000,
        salience: 0.8,
        last_accessed_at: None,
    });

    r.index_l1(&L1DocView {
        id: uuid::Uuid::new_v4(),
        summary: "用户和朋友去吃了火锅，很开心".to_string(),
        keywords: Some("社交,火锅,开心".to_string()),
        persona_uid: Some("user-0001".to_string()),
        created_at: 2000,
        salience: 0.6,
        last_accessed_at: None,
    });

    // 添加 L2 事件
    r.index_l2(&L2DocView {
        id: 1,
        title: "完成Rust项目".to_string(),
        summary: "用户完成了第一个Rust项目，发布了crate".to_string(),
        keywords: Some("Rust,项目,发布".to_string()),
        attitude: Some("感到很有成就感".to_string()),
        paraphrase: Some("对完成重要工作感到满意".to_string()),
        persona_uid: "user-0001".to_string(),
        share: 0.8,
        confidence: 0.9,
        created_at: 1500,
        salience: 0.9,
    });

    r
}

/// 构造仅含指定 L1 文档视图的最小夹具（父模块私有，供后代测试子模块共享）。
fn l1_view_doc(id: uuid::Uuid, summary: &str, created_at: i64) -> L1DocView {
    L1DocView {
        id,
        summary: summary.to_string(),
        keywords: None,
        persona_uid: Some("user-0001".to_string()),
        created_at,
        salience: 0.5,
        last_accessed_at: None,
    }
}

/// 构造指定 id/主体/文本/时间的最小 utt 原文块夹具（父模块私有，供后代测试子模块共享）。
fn make_utt_doc(id: i64, persona_uid: &str, text: &str, created_at: i64) -> UttDocView {
    UttDocView {
        id,
        persona_uid: persona_uid.to_string(),
        session_id: uuid::Uuid::new_v4(),
        block_text: text.to_string(),
        msg_count: 2,
        created_at,
    }
}

mod capacity;
mod dictionary;
mod exact;
mod index;
mod keyword;
mod search;
mod substring;
mod utt;
