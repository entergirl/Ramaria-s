//! crates/ramaria-eval/src/corpus/registry.rs - Ramaria 评测语料注册表
//!
//! 设计特点:
//! - 语料元数据集中登记：名称、版本、许可、来源与每份文件的大小与哈希
//! - 文件按语言区分，文件名保持数据集原样，便于用户对号放置
//! - 注册表只描述文件，不读取内容，也不发起网络请求
//! - 未知语料名返回 `Validation`，文案列出已登记语料

use ramaria_core::error::{RamariaError, RamariaResult};

/// 语料语言版本。
///
/// 字段约定:
/// - `Zh`: 中文版，含中文会话与中文问题；
/// - `En`: 英文版，与中文版同题同序，仅文本翻译不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusLanguage {
    Zh,
    En,
}

impl CorpusLanguage {
    /// 解析语言标签。
    ///
    /// 参数:
    /// - `raw`: 命令行传入的语言标签，只接受 `zh` / `en`（大小写不敏感）。
    ///
    /// 返回:
    /// - 对应语言；其他取值返回 `Validation`。
    pub fn parse(raw: &str) -> RamariaResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "zh" => Ok(Self::Zh),
            "en" => Ok(Self::En),
            other => Err(RamariaError::validation(format!(
                "未知语料语言: {other}（可选: zh / en）"
            ))),
        }
    }

    /// 语言标签（命令行与报告共用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    /// 本地文件名（保持数据集原文件名）。
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Zh => "lifebench_locomo_conversation_format_v2.0_3380QA.json",
            Self::En => "lifebench_locomo_conversation_format_v2.0_3380QA_en.json",
        }
    }
}

/// 单份语料文件的登记项。
///
/// 字段约定:
/// - `language`: 该文件对应语言；
/// - `file_name`: 本地文件名，放在语料根目录下；
/// - `source_path`: 数据集仓库内相对路径，只用于错误提示；
/// - `sha256`: 正文字节的小写十六进制摘要；
/// - `size`: 正文字节数；
/// - `qa_count`: 该版本登记的 QA 总数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusFileSpec {
    pub language: CorpusLanguage,
    pub file_name: &'static str,
    pub source_path: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub qa_count: usize,
}

/// 一份语料的登记项。
///
/// 字段约定:
/// - `name` / `version`: 命令行与指纹使用的稳定标识；
/// - `license`: 数据集许可；
/// - `source_url`: 数据集页地址，只用于文档与错误提示；
/// - `files`: 各语言文件，顺序为 zh、en。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusSpec {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
    pub source_url: &'static str,
    pub files: &'static [CorpusFileSpec],
}

// =========================================================
// LifeBench 2.0
// =========================================================

const LIFEBENCH_ZH: CorpusFileSpec = CorpusFileSpec {
    language: CorpusLanguage::Zh,
    file_name: CorpusLanguage::Zh.file_name(),
    source_path: "locomo_format/lifebench_locomo_conversation_format_v2.0_3380QA.json",
    sha256: "67eb8678fe36953de18c0aa97908259599023827de543c17a4b20c42b08ca4ee",
    size: 27_088_496,
    qa_count: 3380,
};

const LIFEBENCH_EN: CorpusFileSpec = CorpusFileSpec {
    language: CorpusLanguage::En,
    file_name: CorpusLanguage::En.file_name(),
    source_path: "locomo_format/lifebench_locomo_conversation_format_v2.0_3380QA_en.json",
    sha256: "924729a99d469c3db94f90c861662989f8fe19538b2d65ef38b65b382bdf1157",
    size: 30_838_559,
    qa_count: 3380,
};

static LIFEBENCH: CorpusSpec = CorpusSpec {
    name: "lifebench",
    version: "2.0",
    license: "Apache-2.0",
    source_url: "https://huggingface.co/datasets/C1754955896/Lifebenchv2.0",
    files: &[LIFEBENCH_ZH, LIFEBENCH_EN],
};

/// 返回 LifeBench 注册项。
pub fn lifebench() -> &'static CorpusSpec {
    &LIFEBENCH
}

/// 按名称查找注册项。
///
/// 参数:
/// - `name`: 语料名（大小写不敏感，忽略首尾空白）。
///
/// 返回:
/// - 命中返回注册项；未登记返回 `None`。
pub fn find(name: &str) -> Option<&'static CorpusSpec> {
    let normalized = name.trim();
    [lifebench()]
        .into_iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(normalized))
}
