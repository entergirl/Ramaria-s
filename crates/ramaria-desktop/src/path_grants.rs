//! crates/ramaria-desktop/src/path_grants.rs - 原生对话框选取结果的会话级路径授权模块
//!
//! 设计特点:
//! - 选择动作即授权：对话框返回的路径登记为票据，后续导入 / 导出校验凭票据放行
//! - 票据分文件与目录两类：前者对应读取导入源，后者对应写出目标与目录选择
//! - 仅进程内存活（不落盘、不持久化），重启后需重新选择
//! - 列表按上限滚动淘汰最早登记项，防无界增长
//! - 票据只豁免「落在用户主目录白名单内」这一条地理约束，文件形态与链接校验仍照常执行

use std::path::{Path, PathBuf};

use ramaria_core::lock::lock_recover;

/// 单类票据列表的上限（超出后淘汰最早登记的一条）。
const MAX_ENTRIES: usize = 64;

/// 本次会话内经原生对话框显式选择的路径集合。
///
/// 语义:
/// - `files`: 打开对话框选中的文件真实路径，供导入类读取校验；
/// - `dirs`: 保存对话框目标所在父目录与目录对话框选中的目录，供导出类写出校验与目录读取；
/// - 子路径随父目录授权一并生效：选中某目录即视为授权其下全部内容。
#[derive(Debug, Clone, Default)]
pub struct DialogGrants {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl DialogGrants {
    /// 构造空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条文件票据。
    pub fn grant_file(&mut self, real: PathBuf) {
        push_bounded(&mut self.files, real);
    }

    /// 登记一条目录票据。
    pub fn grant_dir(&mut self, real: PathBuf) {
        push_bounded(&mut self.dirs, real);
    }

    /// 该文件是否已被显式授权（自身登记，或落在已授权目录内）。
    pub fn has_file(&self, real: &Path) -> bool {
        self.files.iter().any(|entry| entry == real) || self.has_ancestor_dir(real)
    }

    /// 该目录是否已被显式授权（自身登记，或落在已授权目录内）。
    pub fn has_dir(&self, real: &Path) -> bool {
        self.dirs.iter().any(|entry| entry == real) || self.has_ancestor_dir(real)
    }

    /// 已授权的目录列表（供只读目录集合取并集）。
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// 存在任一已授权目录是该路径的祖先。
    fn has_ancestor_dir(&self, real: &Path) -> bool {
        self.dirs.iter().any(|dir| real.starts_with(dir))
    }
}

/// 取当前会话票据的快照。
///
/// 说明:
/// - 快照用于把授权集合带出锁作用域，避免跨 `.await` 持有 MutexGuard；
/// - 列表受 `MAX_ENTRIES` 约束，拷贝成本可控。
pub fn snapshot(state: &crate::DesktopState) -> DialogGrants {
    lock_recover(&state.dialog_grants, "desktop.dialog_grants").clone()
}

/// 追加一条票据：去重后在超限时淘汰最早登记项。
fn push_bounded(list: &mut Vec<PathBuf>, real: PathBuf) {
    if list.contains(&real) {
        return;
    }
    if list.len() >= MAX_ENTRIES {
        list.remove(0);
    }
    list.push(real);
}

#[cfg(test)]
mod tests;
