//! crates/ramaria-service/src/vision/locate.rs - Ramaria 附件定位与 md5 反查模块
//!
//! 设计特点:
//! - 定位两段：source_ref 相对引用直连（多导出根依次尝试）→ md5 反查兜底
//!   （resources/images/ 文件名前缀匹配）
//! - 反查索引懒建一次（多导出根合并，首个命中根优先），进程内复用
//! - 仅接受安全相对引用（排除绝对路径 / 盘符 / .. / 反斜杠），拒绝越界拼接

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ramaria_core::types::is_local_relative_ref;

// =========================================================
// 定位结果与定位器
// =========================================================

/// 附件定位结果。
pub(crate) struct LocatedFile {
    /// 命中文件的完整路径。
    pub(crate) path: PathBuf,
    /// 归一化后的相对引用（MIME 判定与写回用）。
    pub(crate) normalized_ref: String,
    /// 是否经 md5 反查命中（false = source_ref 直连命中）。
    pub(crate) via_reverse_lookup: bool,
}

/// 附件定位器（多导出根 + md5 反查索引）。
///
/// 状态:
/// - `roots`: 候选导出根目录（构造时去重，保序）；
/// - `image_index`: 反查索引（键 = 文件名小写；值 = 原始文件名 + 完整路径），
///   首次反查时懒建并常驻。
pub(crate) struct AttachmentLocator {
    roots: Vec<PathBuf>,
    image_index: Option<HashMap<String, (String, PathBuf)>>,
}

impl AttachmentLocator {
    /// 构造定位器（根目录去重，保持调用方顺序）。
    ///
    /// 参数:
    /// - `roots`: 候选导出根目录列表。
    pub(crate) fn new(roots: Vec<PathBuf>) -> Self {
        let mut unique: Vec<PathBuf> = Vec::with_capacity(roots.len());
        for root in roots {
            if !unique.contains(&root) {
                unique.push(root);
            }
        }
        Self {
            roots: unique,
            image_index: None,
        }
    }

    /// 定位附件（source_ref 直连优先，md5 反查兜底）。
    ///
    /// 流程:
    /// 1. 直连：安全相对引用逐根拼接并校验为文件，命中即返回；
    /// 2. 反查：md5 非空时按文件名前缀匹配索引候选，路径排序取第一个（确定性）；
    /// 3. 全 miss 返回 None。
    ///
    /// 参数:
    /// - `source_ref`: 附件引用（导出侧相对路径；绝对路径 / 盘符 / .. 等拒绝直连）；
    /// - `md5`: 图片内容指纹（可选；反查兜底用）。
    ///
    /// 返回:
    /// - `Some(LocatedFile)`: 命中定位；`None`: 不可定位。
    pub(crate) fn locate(&mut self, source_ref: &str, md5: Option<&str>) -> Option<LocatedFile> {
        // ---- 1. 直连：安全相对引用逐根尝试（is_absolute 防盘符绕过） ----
        if is_local_relative_ref(source_ref) && !Path::new(source_ref).is_absolute() {
            for root in &self.roots {
                let candidate = root.join(source_ref);
                if candidate.is_file() {
                    return Some(LocatedFile {
                        path: candidate,
                        normalized_ref: source_ref.to_string(),
                        via_reverse_lookup: false,
                    });
                }
            }
        }

        // ---- 2. 反查：md5 前缀匹配文件名（索引懒建） ----
        let md5 = md5
            .map(|value| value.trim().to_lowercase())
            .unwrap_or_default();
        if md5.is_empty() {
            return None;
        }
        self.ensure_image_index();
        let index = self.image_index.as_ref()?;
        let mut hits: Vec<&(String, PathBuf)> = index
            .iter()
            .filter(|(name, _)| name.starts_with(&md5))
            .map(|(_, value)| value)
            .collect();
        hits.sort_by(|a, b| a.1.cmp(&b.1));
        hits.first().map(|(file_name, path)| LocatedFile {
            path: path.clone(),
            normalized_ref: format!("resources/images/{file_name}"),
            via_reverse_lookup: true,
        })
    }

    /// 懒建反查索引（多导出根合并；同文件名首个命中根优先）。
    ///
    /// 说明:
    /// - 逐根遍历 `resources/images/` 目录，目录不存在 / 不可读时跳过该根；
    /// - 仅收录普通文件，文件名转小写作为键。
    fn ensure_image_index(&mut self) {
        if self.image_index.is_some() {
            return;
        }
        let mut index: HashMap<String, (String, PathBuf)> = HashMap::new();
        for root in &self.roots {
            let dir = root.join("resources").join("images");
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = entry
                    .file_name()
                    .to_str()
                    .map(str::to_string)
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                index.entry(name.to_lowercase()).or_insert((name, path));
            }
        }
        self.image_index = Some(index);
    }
}
