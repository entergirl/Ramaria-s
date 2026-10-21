//! crates/ramaria-cli/src/commands/persona/reload.rs - 人格文件目录同步命令
//!
//! 设计特点:
//! - 扫描 personas/ 目录下的 .toml 文件，按文件名（不含扩展名）作为 persona uid 创建或更新记录
//! - `--json` 输出逐文件结果信封（stdout 只含信封）；文本模式输出逐文件结果与加载汇总
//! - 全部文件加载失败时上抛错误，由入口统一映射退出码

use super::{collect_toml_files, personas_dir};
use ramaria_service::{Engine, PersonaFileAction, PersonaLoadMode};
use std::sync::Arc;

/// 从 personas/ 目录重新加载人格文件到 DB。
///
/// 流程:
/// 1. 验证目录存在。
/// 2. 收集 .toml 文件（可按 --uid 筛选）。
/// 3. 逐个文件读取 → 解析 assistant_name → 创建或更新 DB 记录（已存在则同步文件内容）。
/// 4. 输出加载结果摘要。
///
/// 说明:
/// - `--json`: 输出 `{"uid","files","created","updated","failed","results"}` 信封，
///   逐文件结果含文件名 / uid / 动作 / 消息；stdout 只含信封。
/// - 全部文件加载失败时上抛错误（由入口统一映射退出码），不输出成功信封。
pub(super) async fn run(
    engine: &Arc<Engine>,
    uid: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let dir = personas_dir();

    if !dir.exists() {
        return Err(anyhow::anyhow!(
            "人格文件目录不存在: {}\n\
             请确认工作目录在仓库根下，或将 .toml 文件放入 config/personas/ 目录。",
            dir.display()
        ));
    }
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("路径不是目录: {}", dir.display()));
    }

    // 收集要处理的文件（文件名用于逐文件结果展示）
    let files = collect_toml_files(&dir, uid.as_deref())?;

    if files.is_empty() {
        if let Some(ref target_uid) = uid {
            return Err(anyhow::anyhow!(
                "未找到匹配的人格文件: {}.toml (在 {} 下)",
                target_uid,
                dir.display()
            ));
        }
        if json {
            return crate::json::emit_ok(&serde_json::json!({
                "uid": uid,
                "files": 0,
                "created": 0,
                "updated": 0,
                "failed": 0,
                "results": [],
            }));
        }
        crate::ui::warn(&format!("在 {} 下未找到 .toml 文件", dir.display()));
        return Ok(());
    }

    if !json {
        crate::ui::separator();
        println!("  正在加载人格文件... (共 {} 个)", files.len());
        crate::ui::separator();
        println!();
    }

    // 导入用例：已存在记录按文件内容同步（名称 / 配置）
    let outcomes = engine
        .persona_load_from_dir(&dir, uid.as_deref(), PersonaLoadMode::CreateOrUpdate)
        .await?;

    let mut created_count = 0u32;
    let mut updated_count = 0u32;
    let mut error_count = 0u32;
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(files.len());

    for (path, outcome) in files.iter().zip(outcomes.iter()) {
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".to_string());

        match outcome.action {
            PersonaFileAction::Created => {
                if !json {
                    crate::ui::success(&format!("{filename} → {}", outcome.uid));
                }
                created_count += 1;
            }
            PersonaFileAction::Updated => {
                if !json {
                    crate::ui::success(&format!("{filename} → {}", outcome.uid));
                }
                updated_count += 1;
            }
            PersonaFileAction::Failed => {
                // 失败详情只走 stderr 与日志，stdout 保持纯数据
                crate::ui::warn(&format!("{filename}: {}", outcome.message));
                tracing::error!(
                    path = %path.display(),
                    error = %outcome.message,
                    "人格文件加载失败"
                );
                error_count += 1;
            }
            // 其余动作（如仅创建缺失模式的跳过）在本命令口径下不会出现，防御性忽略
            _ => {}
        }

        results.push(serde_json::json!({
            "file": filename,
            "uid": outcome.uid,
            "action": action_label(outcome.action),
            "message": outcome.message,
        }));
    }

    if !json {
        println!();
        crate::ui::separator();
        println!(
            "  加载完成: {} 成功, {} 失败",
            created_count + updated_count,
            error_count
        );
        crate::ui::separator();
    }

    if error_count > 0 && created_count == 0 && updated_count == 0 {
        return Err(anyhow::anyhow!("所有文件加载均失败，请检查文件格式和权限"));
    }

    if json {
        return crate::json::emit_ok(&serde_json::json!({
            "uid": uid,
            "files": files.len(),
            "created": created_count,
            "updated": updated_count,
            "failed": error_count,
            "results": results,
        }));
    }

    Ok(())
}

/// 单文件动作的 JSON 标签（与 `PersonaFileAction` 的序列化口径一致）。
fn action_label(action: PersonaFileAction) -> &'static str {
    match action {
        PersonaFileAction::Created => "created",
        PersonaFileAction::Updated => "updated",
        PersonaFileAction::Skipped => "skipped",
        PersonaFileAction::Failed => "failed",
        _ => "unknown",
    }
}
