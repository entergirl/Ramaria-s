//! crates/ramaria-service/src/import/export_root.rs - Ramaria 导入导出根有效性判定模块
//!
//! 设计特点:
//! - 判定聊天记录导出根：含 `resources` 子目录（附件布局），或根目录存在导出 JSON（纯文本导出）
//! - 供桌面命令入口（目录选择防呆）与"继续处理"编排（无效根跳过附件步骤）共用
//! - 只读检查、无 I/O 副作用；错误文案与日志由调用方负责

use std::path::Path;

/// 判断目录是否为有效的聊天记录导出根。
///
/// 判定条件（任一满足）:
/// - 根目录下存在 `resources` 子目录（qq-chat-exporter 附件布局）；
/// - 根目录（顶层）存在任意 `*.json` 文件（无附件的纯文本导出）。
///
/// 参数:
/// - `root`: 待判定的候选导出根目录。
///
/// 返回:
/// - `true`: 具备导出根特征；`false`: 既无附件目录也无导出 JSON（或目录不可读）。
pub fn is_valid_export_root(root: &Path) -> bool {
    if root.join("resources").is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    /// 含 `resources` 子目录 → 有效导出根。
    #[test]
    fn root_with_resources_is_valid() {
        let dir = temp_dir("export-root-resources");
        std::fs::create_dir_all(dir.join("resources")).expect("创建 resources 目录应成功");
        assert!(is_valid_export_root(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅含导出 JSON（无 resources）→ 有效导出根（兼容纯文本导出）。
    #[test]
    fn root_with_json_file_is_valid() {
        let dir = temp_dir("export-root-json");
        std::fs::write(dir.join("chat.json"), "{}").expect("写入导出 JSON 应成功");
        assert!(is_valid_export_root(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 既无 resources 也无 JSON（含不存在的目录）→ 无效导出根。
    #[test]
    fn root_without_markers_is_invalid() {
        let dir = temp_dir("export-root-plain");
        std::fs::write(dir.join("notes.txt"), "text").expect("写入普通文件应成功");
        assert!(!is_valid_export_root(&dir), "无导出特征目录应判为无效");
        assert!(
            !is_valid_export_root(&dir.join("does-not-exist")),
            "不存在的目录应判为无效"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
