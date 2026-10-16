//! crates/ramaria-cli/src/commands/sticker.rs - 表情资产管理命令
//!
//! 设计特点:
//! - add: 读取本地图片文件写入资产目录并登记（格式与大小校验由服务层执行）
//! - list: 列出全部表情资产（id / 文件名 / 标签 / 描述摘要）
//! - update: 更新描述与标签（描述空串清空；标签出现即整体覆盖，缺省保持）
//! - remove: 删除表情资产（含资产文件的 best-effort 删除）
//! - 摘要只输出元信息，不打印图片内容；--json 信封与文本形态同源

use anyhow::Context;
use ramaria_service::Engine;
use std::path::PathBuf;
use std::sync::Arc;

// =========================================================
// 常量与辅助
// =========================================================

/// 列表与摘要中的描述预览长度上限（字符）。
const DESCRIPTION_PREVIEW_CHARS: usize = 40;

/// 描述预览（压缩空白后截断；无描述回退占位文案）。
fn description_preview(description: Option<&str>) -> String {
    let text: String = description
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return "（无）".to_string();
    }
    ramaria_core::text::truncate_chars(&text, DESCRIPTION_PREVIEW_CHARS)
}

/// 标签展示文本（空列表回退占位文案）。
fn tags_label(tags: &[String]) -> String {
    if tags.is_empty() {
        "（无标签）".to_string()
    } else {
        tags.join("、")
    }
}

// =========================================================
// 公共枚举与入口
// =========================================================

/// Sticker 子命令。
///
/// 职责:
/// - Add: 新增一条表情资产（读文件 + 落盘 + 落库）。
/// - List: 列出全部表情资产。
/// - Update: 更新描述与标签。
/// - Remove: 删除表情资产。
#[derive(Debug, Clone)]
pub enum StickerCmd {
    /// 新增表情资产
    Add {
        /// 图片文件路径
        file: PathBuf,
        /// 资产描述（缺省为空）
        description: Option<String>,
        /// 标签（可重复）
        tags: Vec<String>,
    },
    /// 列出全部表情资产
    List,
    /// 更新表情资产的描述与标签
    Update {
        /// 资产 id
        id: i64,
        /// 新的描述（空串清空；缺省保持）
        description: Option<String>,
        /// 新的标签（出现即覆盖；缺省保持）
        tags: Vec<String>,
    },
    /// 删除表情资产
    Remove {
        /// 资产 id
        id: i64,
    },
}

/// 运行 sticker 子命令分发。
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `cmd`: Sticker 子命令。
/// - `json`: JSON 信封输出。
pub async fn run(engine: &Arc<Engine>, cmd: StickerCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        StickerCmd::Add {
            file,
            description,
            tags,
        } => run_add(engine, file, description, tags, json).await,
        StickerCmd::List => run_list(engine, json).await,
        StickerCmd::Update {
            id,
            description,
            tags,
        } => run_update(engine, id, description, tags, json).await,
        StickerCmd::Remove { id } => run_remove(engine, id, json).await,
    }
}

// =========================================================
// add 命令
// =========================================================

/// 新增表情资产（读取图片文件 → 服务层落盘落库）。
///
/// 说明:
/// - `--json`: 输出资产对象信封；文本模式经 `ui::info` 输出元信息摘要（id /
///   文件名 / 标签数 / 描述长度，不打印图片内容）；
/// - 文件读取失败与文件名校验失败上抛，由入口统一映射退出码。
async fn run_add(
    engine: &Arc<Engine>,
    file: PathBuf,
    description: Option<String>,
    tags: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(&file).with_context(|| format!("读取表情文件失败: {}", file.display()))?;

    let file_name = match file.file_name() {
        Some(name) if !name.is_empty() => name.to_string_lossy().to_string(),
        _ => {
            return Err(anyhow::anyhow!(
                ramaria_core::error::RamariaError::validation("文件路径缺少文件名")
            ));
        }
    };

    let asset = engine
        .sticker_add(&file_name, &bytes, description.as_deref(), &tags)
        .await
        .context("新增表情资产失败")?;

    if json {
        return crate::json::emit_ok(&asset);
    }

    crate::ui::info(&format!(
        "已新增表情资产: id={} 文件名={} 标签 {} 个 描述 {} 字",
        asset.id,
        asset.file_name,
        asset.tags.len(),
        asset
            .description
            .as_deref()
            .map(|text| text.chars().count())
            .unwrap_or(0)
    ));
    Ok(())
}

// =========================================================
// list 命令
// =========================================================

/// 列出全部表情资产。
///
/// 说明:
/// - `--json`: 输出资产数组信封；文本模式逐行输出 id / 文件名 / 标签 / 描述摘要；
/// - 空列表输出提示，成功退出。
async fn run_list(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    let assets = engine.sticker_list().await.context("查询表情资产失败")?;

    if json {
        return crate::json::emit_ok(&assets);
    }

    if assets.is_empty() {
        crate::ui::info("暂无表情资产");
        return Ok(());
    }

    for asset in &assets {
        println!(
            "{}  {}  标签: {}  描述: {}",
            asset.id,
            asset.file_name,
            tags_label(&asset.tags),
            description_preview(asset.description.as_deref())
        );
    }
    Ok(())
}

// =========================================================
// update 命令
// =========================================================

/// 更新表情资产的描述与标签。
///
/// 说明:
/// - 标签出现（非空）即覆盖全部标签，缺省保持；描述原样透传（空串 = 清空）；
/// - `--json`: 输出 `{"id":…,"updated":…}` 信封；文本模式按 updated 两态输出；
/// - 失败错误上抛，由入口统一映射退出码。
async fn run_update(
    engine: &Arc<Engine>,
    id: i64,
    description: Option<String>,
    tags: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let tags = if tags.is_empty() {
        None
    } else {
        Some(tags.as_slice())
    };
    let updated = engine
        .sticker_update(id, description.as_deref(), tags)
        .await
        .context("更新表情资产失败")?;

    if json {
        return crate::json::emit_ok(&serde_json::json!({"id": id, "updated": updated}));
    }

    if updated {
        println!("已更新表情资产: id={id}");
    } else {
        crate::ui::info("未找到匹配的表情资产（无改写）");
    }
    Ok(())
}

// =========================================================
// remove 命令
// =========================================================

/// 删除表情资产。
///
/// 说明:
/// - `--json`: 输出 `{"id":…,"removed":…}` 信封；文本模式按 removed 两态输出；
/// - 失败错误上抛，由入口统一映射退出码。
async fn run_remove(engine: &Arc<Engine>, id: i64, json: bool) -> anyhow::Result<()> {
    let removed = engine
        .sticker_remove(id)
        .await
        .context("删除表情资产失败")?;

    if json {
        return crate::json::emit_ok(&serde_json::json!({"id": id, "removed": removed}));
    }

    if removed {
        println!("已删除表情资产: id={id}");
    } else {
        crate::ui::info("未找到匹配的表情资产（无删除）");
    }
    Ok(())
}
