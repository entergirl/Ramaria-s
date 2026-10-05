//! crates/ramaria-desktop/src/commands/proactive_cmd.rs - 主动消息名单 Tauri Commands
//!
//! 设计特点:
//! - 一对读写命令：名单读取（活跃人格开关状态与生效结论）与开关保存
//! - 委托服务层名单用例，桌面只做视图映射与错误文案转换
//! - 生效结论由服务层单点计算，前端不重算
//! - 日志只记人格标识与开关值等元数据，不记消息内容

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 主动消息名单条目（前端展示用）。
///
/// 字段约定:
/// - `uid` / `name` / `kind`: 人格身份；
/// - `has_local_dialogue`: 本地用户消息存在性（自动态解锁依据，排除导入）；
/// - `mode`: 三态文本 auto / on / off；
/// - `effective`: 生效结论（服务层单点计算）。
#[derive(Debug, Clone, Serialize)]
pub struct ProactivePersonaRow {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub has_local_dialogue: bool,
    pub mode: String,
    pub effective: bool,
}

/// 由服务层名单视图构造前端条目（字段映射的唯一入口，便于单测锁定）。
///
/// 参数:
/// - `item`: 服务层名单条目。
///
/// 返回:
/// - 前端展示用条目（六字段透传，不做任何重算）。
fn row_view(item: ramaria_service::ProactivePersonaView) -> ProactivePersonaRow {
    ProactivePersonaRow {
        uid: item.uid,
        name: item.name,
        kind: item.kind,
        has_local_dialogue: item.has_local_dialogue,
        mode: item.mode,
        effective: item.effective,
    }
}

// =========================================================
// list_proactive_personas — 读取主动消息名单
// =========================================================

/// 列出活跃人格的主动开关状态与生效结论。
///
/// 返回:
/// - JSON 数组，每项为 ProactivePersonaRow（按人格列表顺序）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_proactive_personas(
    state: State<'_, DesktopState>,
) -> Result<Vec<ProactivePersonaRow>, String> {
    let views = state
        .engine
        .proactive_persona_list()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询主动消息名单失败"))?;

    let rows: Vec<ProactivePersonaRow> = views.into_iter().map(row_view).collect();

    tracing::debug!(count = rows.len(), "list_proactive_personas 完成");
    Ok(rows)
}

// =========================================================
// set_proactive_persona — 保存主动开关
// =========================================================

/// 保存指定人格的主动开关（三态）。
///
/// 参数:
/// - `uid`: 人格业务标识。
/// - `mode`: 开关文本（auto / on / off）。
///
/// 返回:
/// - 成功固定返回 `"ok"`；校验 / 存储失败返回用户可读错误文案。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_proactive_persona(
    state: State<'_, DesktopState>,
    uid: String,
    mode: String,
) -> Result<String, String> {
    state
        .engine
        .proactive_persona_set_mode(&uid, &mode)
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "保存主动开关失败"))?;

    tracing::info!(%uid, %mode, "set_proactive_persona 完成");
    Ok("ok".to_string())
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 映射：服务层视图六字段逐项透传（生效结论由服务层计算，前端不重算）。
    #[test]
    fn row_view_maps_fields() {
        let view = ramaria_service::ProactivePersonaView {
            uid: "char-0001".to_string(),
            name: "测试人格".to_string(),
            kind: "char".to_string(),
            has_local_dialogue: true,
            mode: "on".to_string(),
            effective: true,
        };

        let row = row_view(view);
        assert_eq!(row.uid, "char-0001");
        assert_eq!(row.name, "测试人格");
        assert_eq!(row.kind, "char");
        assert!(row.has_local_dialogue, "对话解锁状态应透传");
        assert_eq!(row.mode, "on");
        assert!(row.effective, "生效结论应透传");
    }
}
