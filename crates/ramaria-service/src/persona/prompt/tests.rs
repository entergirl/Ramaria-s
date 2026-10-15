//! crates/ramaria-service/src/persona/prompt/tests.rs - Ramaria 人格提示词预览用例单元测试
//!
//! 设计特点:
//! - 由 `persona::prompt` 以 `#[cfg(test)] mod tests;` 收纳：覆盖静态 / 全量两档内容、
//!   档位边界与只读保证
//! - 使用真实 SQLite 临时库与 mock LLM：预设经导入用例写入后按服务层读取口径断言
//! - 预设文件写入系统临时目录，用例结束清理
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问网络、OS keychain 与真实用户数据。
use std::path::PathBuf;

use ramaria_core::traits::StoreCrud;

use super::*;
use crate::test_support::{engine_with_db, temp_dir};

// =========================================================
// 共享夹具
// =========================================================

/// 预览用例预设：注册列 + traits / facts / examples / 行为规则 + [prompt] 补丁。
const PROMPT_PRESET: &str = r#"[persona]
uid = "char-0031"
kind = "char"
name = "预览小助手"

[identity]
assistant_name = "预览小助手"

[blocks]
A_persona = "角色设定文本。"

[traits]
base = [{ label = "温和", meaning = "对他人保持温和" }]

[[facts]]
field = "BasicInfo"
content = "住在海边小镇"

[[examples]]
user = "今天好累啊"
assistant = "先去休息一下吧"

[[behavior_rules]]
keywords = ["加班", "工作"]
reaction = "表达关心并建议早点休息"

[prompt]
role_suffix = "结尾偶尔加一句关心。"
"#;

/// 写预设文件到独立临时目录（返回文件路径与目录句柄，调用方负责清理）。
fn write_preset(file_name: &str, content: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir("persona-prompt-preset");
    let path = dir.join(file_name);
    std::fs::write(&path, content).expect("写入预设文件应成功");
    (path, dir)
}

/// 导入预览预设并返回引擎（已导入 char-0031）。
async fn engine_with_preset(
    tag: &str,
) -> (
    crate::engine::Engine,
    std::sync::Arc<ramaria_storage::SqliteStorage>,
    PathBuf,
) {
    let (engine, storage, dir) = engine_with_db(tag).await;
    let (path, preset_dir) = write_preset("preview.toml", PROMPT_PRESET);
    engine
        .persona_import(&path, None, true)
        .await
        .expect("导入应成功");
    std::fs::remove_dir_all(preset_dir).ok();
    (engine, storage, dir)
}

// =========================================================
// 静态档 / 全量档
// =========================================================

/// 静态档（无给定输入）：渲染人格名 / trait 标签 / 补丁后缀，且不预选示例、无记忆上下文。
#[tokio::test]
async fn static_tier_renders_profile_and_patch_without_examples() {
    let (engine, _storage, dir) = engine_with_preset("persona-prompt-static").await;

    let preview = preview(&engine, "char-0031", None)
        .await
        .expect("预览应成功");

    assert_eq!(preview.uid, "char-0031");
    assert_eq!(preview.tier, PromptPreviewTier::Static);
    assert!(preview.memory_context.is_none(), "静态档无记忆上下文");
    assert!(
        preview.system_prompt.contains("预览小助手"),
        "含人格名: {}",
        preview.system_prompt
    );
    assert!(
        preview.system_prompt.contains("温和"),
        "含 trait 标签: {}",
        preview.system_prompt
    );
    assert!(
        preview.system_prompt.contains("结尾偶尔加一句关心。"),
        "含 [prompt].role_suffix 正文: {}",
        preview.system_prompt
    );
    assert!(
        !preview.system_prompt.contains("## 对话示例"),
        "静态档不预选示例"
    );
    assert_eq!(preview.chars, preview.system_prompt.chars().count());
    assert!(preview.tokens > 0, "token 估算应为正");

    std::fs::remove_dir_all(dir).ok();
}

/// 两档差异：全量档（给定输入）含示例兜底注入，静态档不含。
#[tokio::test]
async fn full_tier_injects_examples_while_static_does_not() {
    let (engine, _storage, dir) = engine_with_preset("persona-prompt-full").await;

    let static_preview = preview(&engine, "char-0031", None)
        .await
        .expect("静态档预览应成功");
    let full_preview = preview(&engine, "char-0031", Some("今天有点累"))
        .await
        .expect("全量档预览应成功");

    assert_eq!(static_preview.tier, PromptPreviewTier::Static);
    assert_eq!(full_preview.tier, PromptPreviewTier::Full);
    assert!(
        !static_preview.system_prompt.contains("## 对话示例"),
        "静态档不含示例段"
    );
    assert!(
        full_preview.system_prompt.contains("## 对话示例"),
        "全量档（记忆未命中）示例兜底注入: {}",
        full_preview.system_prompt
    );
    assert_eq!(
        full_preview.chars,
        full_preview.system_prompt.chars().count()
    );

    std::fs::remove_dir_all(dir).ok();
}

// =========================================================
// 边界
// =========================================================

/// uid 空 / 未知 uid 返回 Validation；空白输入按静态档。
#[tokio::test]
async fn preview_boundaries() {
    let (engine, _storage, dir) = engine_with_preset("persona-prompt-boundary").await;

    let empty_uid = preview(&engine, "  ", None).await;
    assert!(
        matches!(empty_uid, Err(RamariaError::Validation { .. })),
        "uid 空应返回 Validation: {empty_uid:?}"
    );

    let unknown_uid = preview(&engine, "char-9999", None).await;
    assert!(
        matches!(unknown_uid, Err(RamariaError::Validation { .. })),
        "未知 uid 应返回 Validation: {unknown_uid:?}"
    );

    let blank_input = preview(&engine, "char-0031", Some("   "))
        .await
        .expect("空白输入预览应成功");
    assert_eq!(
        blank_input.tier,
        PromptPreviewTier::Static,
        "空白输入按静态档"
    );
    assert!(blank_input.memory_context.is_none());

    std::fs::remove_dir_all(dir).ok();
}

// =========================================================
// 只读保证
// =========================================================

/// 预览前后会话数与消息数不变（不建会话、不写库）。
#[tokio::test]
async fn preview_is_read_only() {
    let (engine, storage, dir) = engine_with_preset("persona-prompt-readonly").await;

    let sessions_before = storage.list_sessions().await.expect("查询会话应成功").len();
    let messages_before = storage
        .list_messages_by_persona("char-0031")
        .await
        .expect("查询消息应成功")
        .len();

    let _ = preview(&engine, "char-0031", None)
        .await
        .expect("静态档预览应成功");
    let _ = preview(&engine, "char-0031", Some("今天有点累"))
        .await
        .expect("全量档预览应成功");

    let sessions_after = storage.list_sessions().await.expect("查询会话应成功").len();
    let messages_after = storage
        .list_messages_by_persona("char-0031")
        .await
        .expect("查询消息应成功")
        .len();

    assert_eq!(sessions_before, sessions_after, "预览不建会话");
    assert_eq!(messages_before, messages_after, "预览不写消息");

    std::fs::remove_dir_all(dir).ok();
}
