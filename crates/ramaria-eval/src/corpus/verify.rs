//! crates/ramaria-eval/src/corpus/verify.rs - Ramaria 评测语料本地校验
//!
//! 设计特点:
//! - 只读本地文件：存在性、字节数、SHA-256 三项依次校验
//! - 哈希按 64 KiB 分块流式计算，校验阶段不整份读入内存
//! - 失败文案带期望路径、来源 URL、期望哈希与实际值（存在时）
//! - 中英两份同时校验时追加 question_id 对齐检查
//! - 校验不发起任何网络请求

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use sha2::{Digest, Sha256};

use super::lifebench::{self, AlignmentReport};
use super::registry::{CorpusFileSpec, CorpusLanguage, CorpusSpec, find, lifebench};

/// 语料根目录（相对当前工作目录）。
const CORPUS_ROOT: &str = ".eval/corpus/lifebench/2.0";
/// 哈希计算的分块大小。
const HASH_CHUNK: usize = 64 * 1024;

/// 单份文件的校验结果。
///
/// 字段约定:
/// - `path`: 期望的本地路径；
/// - `expected_size` / `actual_size`: 登记字节数与实际字节数；
/// - `expected_sha256` / `actual_sha256`: 登记摘要与实际摘要；
/// - `matched`: 两项均与登记一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCheck {
    pub language: CorpusLanguage,
    pub source_path: &'static str,
    pub path: PathBuf,
    pub expected_size: u64,
    pub actual_size: u64,
    pub expected_sha256: &'static str,
    pub actual_sha256: String,
    pub matched: bool,
}

/// 语料校验报告。
///
/// 字段约定:
/// - `files`: 本次校验的每份文件，顺序同注册表；
/// - `alignment`: 中英两份都校验时的对齐结果，否则为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub corpus: &'static str,
    pub version: &'static str,
    pub license: &'static str,
    pub source_url: &'static str,
    pub files: Vec<FileCheck>,
    pub alignment: Option<AlignmentReport>,
}

impl VerifyReport {
    /// 渲染为人读报告（每行一条事实，供 CLI 直接输出）。
    pub fn render(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "语料: {} {}（{}）",
            self.corpus, self.version, self.license
        );
        let _ = writeln!(output, "来源: {}", self.source_url);
        for file in &self.files {
            let _ = writeln!(
                output,
                "  [{}] {}",
                file.language.as_str(),
                file.path.display()
            );
            let _ = writeln!(
                output,
                "       字节: {}（期望 {}）",
                file.actual_size, file.expected_size
            );
            let _ = writeln!(
                output,
                "       SHA-256: {}（期望 {}）",
                file.actual_sha256, file.expected_sha256
            );
            let _ = writeln!(
                output,
                "       {}",
                if file.matched { "匹配" } else { "不匹配" }
            );
        }
        match &self.alignment {
            Some(report) => {
                let _ = writeln!(
                    output,
                    "对齐: {} samples / {} QA，question_id 与 gold 字段一致",
                    report.samples, report.qa_count
                );
            }
            None => {
                let _ = writeln!(output, "对齐: 未执行（需同时校验中英两份文件）");
            }
        }
        output
    }
}

// =========================================================
// 路径
// =========================================================

/// 返回默认语料根目录：当前工作目录下的 `.eval/corpus/lifebench/2.0`。
///
/// 返回:
/// - 成功时返回绝对路径（仅拼接，不创建目录）；
/// - 无法读取当前目录返回 `Io` 错误。
pub fn default_root() -> RamariaResult<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))?;
    Ok(cwd.join(CORPUS_ROOT))
}

/// 计算某语言文件的期望本地路径。
///
/// 参数:
/// - `spec`: 语料注册项；
/// - `language`: 目标语言。
///
/// 返回:
/// - 注册项未登记该语言时返回 `Validation`。
pub fn expected_path(spec: &CorpusSpec, language: CorpusLanguage) -> RamariaResult<PathBuf> {
    let file = spec
        .files
        .iter()
        .find(|file| file.language == language)
        .ok_or_else(|| unknown_language(spec, language))?;
    Ok(default_root()?.join(file.file_name))
}

// =========================================================
// 校验
// =========================================================

/// 校验语料本地副本。
///
/// 参数:
/// - `name`: 语料名；
/// - `language`: 目标语言，`None` 表示注册表内全部语言。
///
/// 返回:
/// - 全部通过返回报告；文件缺失、字节数或摘要不符、中英不对齐返回 `Validation`；
/// - 读取失败返回 `Io`。
pub fn verify(name: &str, language: Option<CorpusLanguage>) -> RamariaResult<VerifyReport> {
    let spec = find(name).ok_or_else(|| {
        RamariaError::validation(format!("未知语料: {name}（已登记: {}）", lifebench().name))
    })?;
    let root = default_root()?;
    let selected: Vec<&CorpusFileSpec> = spec
        .files
        .iter()
        .filter(|file| language.is_none_or(|target| file.language == target))
        .collect();
    if selected.is_empty() {
        return Err(language.map_or_else(
            || RamariaError::validation(format!("语料 {} 未登记任何文件", spec.name)),
            |target| unknown_language(spec, target),
        ));
    }

    let mut files = Vec::with_capacity(selected.len());
    for file in &selected {
        let path = root.join(file.file_name);
        files.push(check_file(spec, file, &path)?);
    }

    let has_zh = selected
        .iter()
        .any(|file| file.language == CorpusLanguage::Zh);
    let has_en = selected
        .iter()
        .any(|file| file.language == CorpusLanguage::En);
    let alignment = if has_zh && has_en {
        Some(align_pair(&root)?)
    } else {
        None
    };

    Ok(VerifyReport {
        corpus: spec.name,
        version: spec.version,
        license: spec.license,
        source_url: spec.source_url,
        files,
        alignment,
    })
}

/// 校验语料并打印报告到标准输出。
///
/// 参数:
/// - `name`: 语料名；
/// - `language`: 语言标签，`None` 表示全部语言。
///
/// 返回:
/// - 标签非法与校验失败的返回同 [`verify`]。
pub fn verify_command(name: &str, language: Option<&str>) -> RamariaResult<()> {
    let language = language.map(CorpusLanguage::parse).transpose()?;
    let report = verify(name, language)?;
    println!("{}", report.render());
    Ok(())
}

/// 依次校验存在性、字节数与摘要。
///
/// 参数:
/// - `spec`: 语料注册项（来源 URL 用于错误文案）；
/// - `file`: 文件登记项；
/// - `path`: 本地路径。
pub(super) fn check_file(
    spec: &CorpusSpec,
    file: &CorpusFileSpec,
    path: &Path,
) -> RamariaResult<FileCheck> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RamariaError::validation(format!(
                "语料文件缺失: {}（从 {} 下载 {}；期望 {} B，期望 SHA-256 {}）",
                path.display(),
                spec.source_url,
                file.source_path,
                file.size,
                file.sha256
            ))
        } else {
            RamariaError::io(
                format!("读取语料文件信息失败: {}", path.display()),
                Some(error),
            )
        }
    })?;

    let actual_size = metadata.len();
    if actual_size != file.size {
        return Err(RamariaError::validation(format!(
            "语料文件大小不符: {}（期望 {} B，实际 {} B；来源 {} 的 {}；期望 SHA-256 {}）",
            path.display(),
            file.size,
            actual_size,
            spec.source_url,
            file.source_path,
            file.sha256
        )));
    }

    let (hashed_size, actual_sha256) = sha256_file(path)?;
    if !actual_sha256.eq_ignore_ascii_case(file.sha256) {
        return Err(RamariaError::validation(format!(
            "语料文件 SHA-256 不符: {}（期望 {}，实际 {}；来源 {} 的 {}）",
            path.display(),
            file.sha256,
            actual_sha256,
            spec.source_url,
            file.source_path
        )));
    }

    Ok(FileCheck {
        language: file.language,
        source_path: file.source_path,
        path: path.to_path_buf(),
        expected_size: file.size,
        actual_size: hashed_size,
        expected_sha256: file.sha256,
        actual_sha256,
        matched: true,
    })
}

/// 分块计算文件 SHA-256，返回字节数与十六进制摘要。
fn sha256_file(path: &Path) -> RamariaResult<(u64, String)> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        RamariaError::io(format!("打开语料文件失败: {}", path.display()), Some(error))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            RamariaError::io(
                format!("计算语料文件摘要时读取失败: {}", path.display()),
                Some(error),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

/// 中英两份都通过文件校验后做逐题对齐。
fn align_pair(root: &Path) -> RamariaResult<AlignmentReport> {
    let zh = lifebench::load_language(root, CorpusLanguage::Zh)?;
    let en = lifebench::load_language(root, CorpusLanguage::En)?;
    lifebench::align(&zh, &en)
}

/// 构造「注册表未登记该语言」错误。
fn unknown_language(spec: &CorpusSpec, language: CorpusLanguage) -> RamariaError {
    RamariaError::validation(format!(
        "语料 {} 未登记 {} 版本",
        spec.name,
        language.as_str()
    ))
}
