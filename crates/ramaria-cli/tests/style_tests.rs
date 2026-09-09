//! tests/style_tests.rs - 表达层风格统计命令测试（style update）
//!
//! 覆盖:
//! - update: 空 persona / 无消息 → 不报错，落库 status=Insufficient 且不生成规则
//! - update: 未指定 persona 回退默认 persona（rama-0001），空数据同样不报错
//!
//! 说明:
//! - 核心（`style_incremental_update_core`）按 persona 全量消息计算五维统计，
//!   MockStorage 的 `list_messages_by_persona` 恒返回空（零消息冷启动路径）。
//! - 零消息 → 样本量 0 < 阈值 200 → 数据不足（Insufficient），不调用 LLM、不生成规则。
//!
//! 安全约束:
//! - 全部使用 MockStorage + MockLlm，不触碰真实数据库/LLM。
//! - clap 命令定义在 main.rs（进程级解析由单元测试覆盖），
//!   本文件直接测 `commands::style::run` 分发逻辑。

mod common;

use common::build_test_app;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::StyleStatsStatus;

use ramaria_cli::commands::style::{StyleCmd, run};

// =========================================================
// update
// =========================================================

/// 空 persona / 无消息 → 风格统计不报错，落库 status=Insufficient、无规则文本。
///
/// 覆盖手动补跑入口的核心契约：无数据 persona 不得被当作错误（与 relearn 同语义），
/// 且统计记录确实写回 `persona_style_stats` 供后续达阈值时自动恢复规则生成。
#[tokio::test]
async fn style_update_empty_persona_ok_insufficient() {
    let (app, storage) = build_test_app();
    let cmd = StyleCmd::Update {
        persona: Some("rama-0001".to_string()),
    };
    run(&app, cmd, true)
        .await
        .expect("空 persona 风格统计不应报错（零消息冷启动）");

    let record = storage
        .get_style_stats("rama-0001")
        .await
        .expect("读取统计成功")
        .expect("应有统计记录");
    assert_eq!(record.persona_uid, "rama-0001");
    assert_eq!(
        record.status,
        StyleStatsStatus::Insufficient,
        "零消息应标注 Insufficient"
    );
    assert!(record.rule_text.is_none(), "Insufficient 不生成自动规则");
    assert_eq!(record.sample_count, 0, "零消息样本量为 0");
}

/// 未指定 persona → 回退默认 persona（rama-0001），空数据同样不报错。
#[tokio::test]
async fn style_update_default_persona_ok() {
    let (app, _storage) = build_test_app();
    let cmd = StyleCmd::Update { persona: None };
    run(&app, cmd, true)
        .await
        .expect("默认 persona 空数据风格统计不应报错");
}
