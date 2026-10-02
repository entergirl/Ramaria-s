//! crates/ramaria-llm/src/model_manager/presets.rs - 嵌入模型预置与常量
//!
//! 设计特点:
//! - `DEFAULT_MODEL_ID` / `MODEL_PRESETS`: 支持 bge-small-zh-v1.5 与 Qwen3-Embedding-0.6B
//! - 预置的 SHA-256 校验和绑定 HuggingFace 仓库 `main` 当前 revision 的文件内容
//! - 上游更新文件后必须同步更新预置校验和
//! - 必需文件清单用于模型就绪检查与下载完整性验证

// =========================================================
// 常量
// =========================================================

/// 默认嵌入模型标识
pub const DEFAULT_MODEL_ID: &str = "bge-small-zh-v1.5";

/// 支持的模型清单
///
/// 每个模型包含:
/// - model_id: 模型目录名
/// - hf_repo: HuggingFace 仓库路径（org/repo）
/// - files: 需要下载的文件列表（文件名 + SHA-256；空串 = 跳过校验）
/// - estimated_size: 预估下载大小（字节）
///
/// 说明:
/// - files 中的校验和绑定 HuggingFace 仓库 `main` 当前 revision 的文件内容；
///   上游更新文件后必须同步更新预置校验和。
#[derive(Debug, Clone)]
pub struct ModelPreset {
    pub model_id: &'static str,
    pub hf_repo: &'static str,
    pub files: &'static [(&'static str, &'static str)],
    /// 模型预估大小（字节），用于 UI 展示
    pub estimated_size: u64,
}

/// 预置模型配置。
pub const MODEL_PRESETS: &[ModelPreset] = &[
    // ---- bge-small-zh-v1.5 (BERT 架构, 384维, ~100MB) ----
    ModelPreset {
        model_id: "bge-small-zh-v1.5",
        hf_repo: "BAAI/bge-small-zh-v1.5",
        files: &[
            (
                "config.json",
                "3853a7979202c348751b753e36f579c41d8da7d36af617d3d907e1fc9b441f2a",
            ),
            (
                "tokenizer.json",
                "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26",
            ),
            (
                "model.safetensors",
                "354763b9b1357bc9c44f62c6be2276321081ed2567773608c0d0785b61d5a026",
            ),
        ],
        estimated_size: 100_000_000,
    },
    // ---- Qwen3-Embedding-0.6B (LLaMA/Qwen3 架构, 1024维, ~1.2GB) ----
    ModelPreset {
        model_id: "Qwen3-Embedding-0.6B",
        hf_repo: "Qwen/Qwen3-Embedding-0.6B",
        files: &[
            (
                "config.json",
                "b5bf1f51fc45be473a54718cef92448d90a1be001bf9b9a44b8c7f10a19feaa9",
            ),
            (
                "tokenizer.json",
                "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a",
            ),
            // 当前 main 为单文件 model.safetensors（约 1.19GB），预置按单文件下载并已绑定其
            // SHA-256；若上游改为分片 safetensors 需同步更新预置。
            (
                "model.safetensors",
                "0437e45c94563b09e13cb7a64478fc406947a93cb34a7e05870fc8dcd48e23fd",
            ),
        ],
        estimated_size: 1_200_000_000,
    },
];

/// 默认 HuggingFace 域名
pub(crate) const HF_DOMAIN: &str = "https://huggingface.co";

/// 下载缓冲区大小（64KB）— 供未来手动缓冲实现使用
const _DOWNLOAD_BUF_SIZE: usize = 64 * 1024;

/// 下载临时文件后缀
pub(crate) const TEMP_SUFFIX: &str = ".part";

/// 必需文件列表（用于就绪检查）
pub(crate) const REQUIRED_FILES: &[&str] = &["config.json", "model.safetensors", "tokenizer.json"];
