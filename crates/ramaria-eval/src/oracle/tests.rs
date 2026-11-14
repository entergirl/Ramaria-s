//! crates/ramaria-eval/src/oracle/tests.rs - Ramaria 压缩层 oracle 入口单元测试
//!
//! 设计特点:
//! - 矩阵改写是纯函数：只锁定管线轴替换，其余轴逐项相等
//! - 运行状态缺失走结构化错误，文案带期望路径，不触碰文件系统其余部分
//! - 不装配引擎、不读语料、不写 .eval 产物

use ramaria_core::error::RamariaError;

use crate::run::{AnswerModelKind, Pipeline, RunMatrix};

use super::*;

fn matrix() -> RunMatrix {
    serde_json::from_value(serde_json::json!({
        "run": {
            "corpus": "lifebench",
            "language": "zh",
            "tiers": ["u1-m1"],
            "pipelines": ["layered", "flat-rag"],
            "budgets": [800],
            "question_limit": 3,
            "strict_isolation": true,
            "retry_limit": 1
        },
        "answer_models": [{ "kind": "local", "base_url": "", "model": "m" }]
    }))
    .expect("矩阵夹具应可构造")
}

/// 冻结矩阵只替换管线轴，档位 / 预算 / 模型与运行选项保持不变。
#[test]
fn freeze_replaces_pipeline_axis() {
    let frozen = freeze(matrix());
    assert_eq!(frozen.run.pipelines, vec![Pipeline::OracleL1]);
    assert_eq!(frozen.run.tiers, vec!["u1-m1".to_string()]);
    assert_eq!(frozen.run.budgets, vec![800]);
    assert_eq!(frozen.run.question_limit, Some(3));
    assert!(frozen.run.strict_isolation);
    assert_eq!(frozen.answer_models[0].kind, AnswerModelKind::Local);
    assert_eq!(frozen.answer_models[0].model, "m");
}

/// 被引用运行不存在时返回结构化错误，文案带期望路径。
#[tokio::test]
async fn run_rejects_missing_state() {
    let id = format!("ramaria-eval-oracle-missing-{}", std::process::id());
    let error = run(&id, None).await.expect_err("状态缺失应报错");
    assert!(
        matches!(&error, RamariaError::Io { .. }),
        "应为 Io，实际 {error:?}"
    );
    assert!(
        error.to_string().contains(&id),
        "文案应带运行标识，实际 {error}"
    );
}
