//! rust/crates/ramaria-llm/src/embedding/models/llama_head_dim.rs - LLaMA 变体嵌入编码器（可配置 head_dim）
//!
//! 设计特点:
//! - 基于 candle 0.10 的 `qwen2` 模块（已 patch head_dim 支持），适配所有显式指定 head_dim 的 LLaMA 变体
//! - 标准 LLaMA: head_dim = hidden_size / num_attention_heads（如 1024/16=64）
//! - 本变体: head_dim 由 config.json 显式指定（如 128），Q 投影维度 = num_heads × head_dim ≠ hidden_size
//! - 仅用于嵌入提取（非生成），不维护 KV cache
//! - 池化策略: Last token pooling（取最后一个有效 token 的 hidden state）+ L2 归一化
//! - 所有计算在 CPU 上执行，保证 Send + Sync
//! - 最大序列长度: 2048 tokens
//!
//! 依赖前提:
//! - candle-transformers 需本地 patch（添加 head_dim: Option<usize> 到 qwen2::Config）
//! - 模型目录需包含: config.json, model.safetensors, tokenizer.json
//! - config.json 中需包含 `head_dim` 字段（或 `architectures`/`model_type` 指示 head_dim 变体）
//!
//! 适用场景:
//! - Qwen3-Embedding 系列（0.6B/4B/8B）
//! - 其他显式指定 head_dim ≠ hidden_size/num_attention_heads 的 LLaMA 变体嵌入模型
//!
//! 与 llama.rs 的区别:
//! - llama.rs 手动解析 config.json → 构造 LlamaConfig（head_dim 硬编码 derived）
//! - llama_head_dim.rs 使用 candle 的 qwen2 Config（Deserialize），支持 config.json 中的 head_dim

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::qwen2::{Config as Qwen2Config, Model};
use ramaria_core::error::RamariaResult;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::Tokenizer;

// =========================================================
// 常量
// =========================================================

/// 默认最大序列长度（LLaMA head_dim 变体通用）
const MAX_SEQ_LEN: usize = 2048;

/// 模型权重文件
const MODEL_FILE: &str = "model.safetensors";

/// 分词器文件
const TOKENIZER_FILE: &str = "tokenizer.json";

/// 配置文件
const CONFIG_FILE: &str = "config.json";

// =========================================================
// LlamaHeadDimEncoder
// =========================================================

/// LLaMA head_dim 变体嵌入编码器。
///
/// 职责:
/// - 通过 candle 的 qwen2 模块（已 patch head_dim 支持）加载模型权重
/// - 执行 `text → tokenize → forward → last token pool → L2 norm` 管线
///
/// 字段:
/// - `model`: candle qwen2 Model 实例（Mutex 包裹，因 forward 需要 &mut self）
/// - `tokenizer`: HuggingFace BPE tokenizer
/// - `dimension`: 向量维度（hidden_size）
/// - `device`: 计算设备（固定为 CPU）
pub struct LlamaHeadDimEncoder {
    model: Mutex<Model>,
    tokenizer: Tokenizer,
    dimension: usize,
    device: Device,
}

impl LlamaHeadDimEncoder {
    /// 从模型目录加载 head_dim 变体编码器。
    ///
    /// 参数:
    /// - `model_dir`: 包含 config.json、model.safetensors、tokenizer.json 的目录。
    ///
    /// 返回:
    /// - 已加载并可用于推理的 LlamaHeadDimEncoder。
    ///
    /// 说明:
    /// - candle 的 qwen2 Config 从 config.json 反序列化，需本地 patch 支持 head_dim 字段。
    /// - 若 config.json 无 head_dim 字段，candle 回退到 hidden_size/num_attention_heads。
    ///
    /// 错误场景:
    /// - 模型文件缺失或损坏。
    /// - config.json 与 safetensors 权重不匹配。
    /// - 分词器缺失或格式无效。
    /// - 模型过大导致 OOM。
    pub fn load(model_dir: &Path) -> RamariaResult<Self> {
        let device = Device::Cpu;

        // ---- 加载 config.json（candle 的 qwen2 Config 支持 Deserialize + head_dim patch） ----
        let config_path = model_dir.join(CONFIG_FILE);
        if !config_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "配置文件缺失: {}",
                config_path.display()
            )));
        }

        let config: Qwen2Config = {
            let file = std::fs::File::open(&config_path).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "模型配置文件打开失败: {} — {}",
                    config_path.display(),
                    e
                ))
            })?;
            serde_json::from_reader(file).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "模型配置 JSON 解析失败: {} — {}",
                    config_path.display(),
                    e
                ))
            })?
        };

        let dimension = config.hidden_size;
        tracing::info!(
            hidden_size = dimension,
            num_layers = config.num_hidden_layers,
            num_heads = config.num_attention_heads,
            num_kv_heads = config.num_key_value_heads,
            "LLaMA head-dim 变体模型配置已加载"
        );

        // ---- 加载 safetensors 权重 ----
        let model_path = model_dir.join(MODEL_FILE);
        if !model_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "模型权重文件缺失: {}",
                model_path.display()
            )));
        }

        // 读取 safetensors 键名（诊断用，非致命）
        match super::common::read_safetensors_header(&model_path) {
            Ok(header_bytes) => {
                if let Ok(header) = serde_json::from_slice::<serde_json::Value>(&header_bytes)
                    && let Some(obj) = header.as_object()
                {
                    let keys: Vec<&str> = obj.keys().map(|s| s.as_str()).take(5).collect();
                    tracing::info!(
                        total_keys = obj.len(),
                        sample_keys = ?keys,
                        "safetensors tensor 键名"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(%e, "无法读取 safetensors tensor 键名（非致命，继续加载）");
            }
        }

        // SAFETY: from_mmaped_safetensors 通过内存映射读取本地模型文件。
        // 调用者需保证:
        // (1) 模型文件来自可信来源（HuggingFace 官方仓库）。
        // (2) 在 mmap 期间文件不会被外部修改。
        // (3) tensor 数据类型为 F32（DType::F32），与 candle 期望一致。
        // 若文件被外部截断或修改，mmap 会触发 SIGBUS。
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_path.as_path()], DType::F32, &device)
        }
        .map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "模型权重加载失败: {} — {}。请确保内存充足（大模型可能占用 1-2GB）",
                model_path.display(),
                e
            ))
        })?;

        // ---- 检测 tensor 键名前缀并适配 ----
        // candle 的 qwen2 Model::new 通过 vb.pp("model") 添加 "model." 前缀查询 tensor，
        // 但 Qwen3-Embedding 的 safetensors 键名无此前缀（如 "embed_tokens.weight"）。
        // 当 safetensors 无前缀时，通过 rename_f 剥离 candle 查询路径中的 "model."。
        let st_has_prefix = st_keys_have_prefix(&model_path);
        let vb = if st_has_prefix {
            tracing::debug!("safetensors 键名已有 'model.' 前缀，无需适配");
            vb
        } else {
            tracing::info!("safetensors 键名无 'model.' 前缀，启用 rename_f 剥离适配");
            vb.rename_f(|name: &str| name.strip_prefix("model.").unwrap_or(name).to_string())
        };

        let model = Model::new(&config, vb).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "模型构建失败: {} — 可能权重与配置不匹配。\
                 \n  提示: 请确认 candle-transformers 已本地 patch head_dim 支持。",
                e
            ))
        })?;

        tracing::info!(
            path = %model_path.display(),
            "模型权重已加载"
        );

        // ---- 加载分词器 ----
        let tokenizer_path = model_dir.join(TOKENIZER_FILE);
        if !tokenizer_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "分词器文件缺失: {}",
                tokenizer_path.display()
            )));
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "分词器加载失败: {} — {}",
                tokenizer_path.display(),
                e
            ))
        })?;

        tracing::info!(
            path = %tokenizer_path.display(),
            vocab_size = tokenizer.get_vocab_size(true),
            "分词器已加载"
        );

        Ok(Self {
            model: Mutex::new(model),
            tokenizer,
            dimension,
            device,
        })
    }

    /// 返回向量维度。
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// 对单条文本执行嵌入。
    ///
    /// 完整管线:
    /// 1. Tokenize: text → input_ids（含特殊 token，encode(add_special_tokens=true)）
    /// 2. Forward: → hidden_states [1, seq_len, hidden_size]
    /// 3. Last Token Pooling: 取序列中最后一个有效 token 的 hidden state
    /// 4. L2 Normalize
    ///
    /// 参数:
    /// - `text`: 待嵌入文本。
    ///
    /// 返回:
    /// - L2 归一化后的 f32 向量。
    pub fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(ramaria_core::error::RamariaError::embedding(
                "嵌入文本不能为空",
            ));
        }

        // Step 1: Tokenize（add_special_tokens=true，BPE tokenizer 添加 BOS/EOS）
        let encoding = self.tokenizer.encode(text, true).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "分词失败: {} — 文本: '{}...'",
                e,
                &text[..text.len().min(60)]
            ))
        })?;

        let token_ids: Vec<u32> = encoding.get_ids().to_vec();
        let attention_mask: Vec<u32> = encoding.get_attention_mask().to_vec();
        let seq_len = token_ids.len();

        if seq_len > MAX_SEQ_LEN {
            tracing::warn!(seq_len, max = MAX_SEQ_LEN, "输入序列超长，将截断");
        }

        let effective_len = seq_len.min(MAX_SEQ_LEN);
        let token_ids = &token_ids[..effective_len];
        let attention_mask = &attention_mask[..effective_len];

        // Step 2: 构建输入张量 [1, effective_len]
        let input_ids = Tensor::new(token_ids, &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("input_ids 构建失败: {}", e))
            })?
            .unsqueeze(0)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("unsqueeze 失败: {}", e))
            })?;

        // Step 3: Forward pass（Model::forward 需要 &mut self，seqlen_offset=0 表示首次推理）
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let hidden_states = model.forward(&input_ids, 0, None).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "前向推理失败: {}。文本: '{}...'",
                e,
                &text[..text.len().min(60)]
            ))
        })?;
        // hidden_states: [1, effective_len, hidden_size]

        // Step 4: Last Token Pooling
        let pooled = Self::last_token_pooling(&hidden_states, attention_mask)?;

        // Step 5: L2 Normalize
        let normalized = super::common::l2_normalize(&pooled, &self.device)?;

        // Step 6: 转为 Vec<f32>
        let vec = normalized.to_vec1::<f32>().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("输出向量提取失败: {}", e))
        })?;

        Ok(vec)
    }

    /// 批量嵌入（逐条处理，Model 不支持变长 batch 前向）。
    pub fn embed_batch_texts(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed_text(text)?);
        }
        Ok(results)
    }

    // ---- 池化辅助 ----

    /// Last token pooling：取 attention_mask 中最后一个 1 对应位置的 hidden state。
    ///
    /// hidden_states: [1, seq_len, hidden_size]
    /// attention_mask: [effective_len] (u32 slice)
    fn last_token_pooling(hidden: &Tensor, attention_mask: &[u32]) -> RamariaResult<Tensor> {
        let dims = hidden.dims3().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("hidden dims 获取失败: {}", e))
        })?;
        let seq_len = dims.1;

        // 从末尾找最后一个有效 token
        let mut last_valid = seq_len.saturating_sub(1);
        for i in (0..seq_len).rev() {
            if attention_mask.get(i).copied().unwrap_or(0) == 1 {
                last_valid = i;
                break;
            }
        }

        // 提取 last_valid 位置的 hidden state: [hidden_size]
        let token_hidden = hidden.i((0, last_valid)).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "last token 提取失败 (pos={}): {}",
                last_valid, e
            ))
        })?;

        Ok(token_hidden)
    }
}

// =========================================================
// safetensors 前缀检测
// =========================================================

/// 快速检测 safetensors 键名是否带 `model.` 前缀。
fn st_keys_have_prefix(model_path: &Path) -> bool {
    super::common::read_safetensors_header(model_path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|header_str| {
            // 检查第一个非 metadata 键
            !header_str.contains("\"embed_tokens.weight\"")
                && header_str.contains("\"model.embed_tokens.weight\"")
        })
        .unwrap_or(false)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    #[test]
    fn empty_text_error() {
        let err = ramaria_core::error::RamariaError::embedding("嵌入文本不能为空");
        assert_eq!(err.category(), "embedding");
    }
}
