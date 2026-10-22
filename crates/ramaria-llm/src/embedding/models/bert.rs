//! crates/ramaria-llm/src/embedding/models/bert.rs - BERT 编码器（bge-small-zh-v1.5）
//!
//! 设计特点:
//! - 基于 `candle-transformers` 的 `BertModel`，支持从 safetensors 直接加载权重
//! - 仅使用 encoder（BERT 无 decoder），适用于嵌入提取场景
//! - 池化策略: Mean pooling（attention_mask 加权平均）+ L2 归一化
//! - 最大序列长度: 512 tokens（bge-small-zh-v1.5 标准）
//! - 计算设备由调用方传入（CPU 或 CUDA GPU），权重经 `VarBuilder` 直接加载到目标设备
//! - 输入使用 i64 token IDs（与 HuggingFace tokenizers 输出对齐）

mod config;
mod pooling;

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::BertModel;
use ramaria_core::error::RamariaResult;
use std::path::Path;
use tokenizers::Tokenizer;

// =========================================================
// 常量
// =========================================================

/// BERT 默认最大序列长度（CLS + tokens + SEP）
const MAX_SEQ_LEN: usize = 512;

/// 模型文件名
const MODEL_FILE: &str = "model.safetensors";

/// 分词器文件名
const TOKENIZER_FILE: &str = "tokenizer.json";

/// 配置文件
const CONFIG_FILE: &str = "config.json";

// =========================================================
// BertEncoder
// =========================================================

/// BERT 嵌入编码器。
///
/// 职责:
/// - 加载 BERT 模型权重（safetensors 格式）和分词器
/// - 执行 `text → tokenize → forward → mean pool → L2 norm` 管线
///
/// 字段:
/// - `model`: candle BertModel 实例
/// - `tokenizer`: HuggingFace tokenizer
/// - `dimension`: 向量维度（与 BERT hidden_size 一致）
/// - `device`: 计算设备（CPU 或 CUDA GPU，由调用方传入）
pub struct BertEncoder {
    model: BertModel,
    tokenizer: Tokenizer,
    dimension: usize,
    device: Device,
}

impl BertEncoder {
    /// 从模型目录加载 BERT 编码器。
    ///
    /// 参数:
    /// - `model_dir`: 包含 config.json、model.safetensors、tokenizer.json 的目录。
    /// - `device`: 计算设备（CPU 或 CUDA GPU），权重将加载到该设备。
    ///
    /// 返回:
    /// - 已加载并可用于推理的 BertEncoder。
    ///
    /// 错误场景:
    /// - 模型文件缺失。
    /// - config.json 解析失败。
    /// - safetensors 权重加载失败（文件损坏、键名不匹配）。
    /// - 分词器加载失败。
    pub fn load(model_dir: &Path, device: &Device) -> RamariaResult<Self> {
        // ---- 加载 config.json ----
        let config_path = model_dir.join(CONFIG_FILE);
        let config = config::parse_config(&config_path)?;

        let dimension = config.hidden_size;
        tracing::info!(
            hidden_size = dimension,
            num_layers = config.num_hidden_layers,
            num_heads = config.num_attention_heads,
            "BERT 配置已加载"
        );

        // ---- 加载 safetensors 权重 ----
        let model_path = model_dir.join(MODEL_FILE);
        if !model_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "模型权重文件缺失: {}。请确保模型目录包含 model.safetensors",
                model_path.display()
            )));
        }

        // SAFETY: from_mmaped_safetensors 通过内存映射读取本地模型文件。
        // 调用者需保证:
        // (1) 模型文件来自可信来源（用户自行下载或通过 ModelManager 校验的 HuggingFace 文件）。
        // (2) 在 mmap 期间文件不会被外部修改（Ramaria 进程独占模型文件写权限）。
        // (3) tensor 数据类型为 F32（DType::F32），与 candle 期望一致。
        // 若文件被外部截断或修改，mmap 会触发 SIGBUS。此风险由用户操作模型文件的行为承担，
        // 属于本地部署场景的可接受边界。
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_path.as_path()], DType::F32, device)
        }
        .map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "BERT 权重加载失败: {} — {}。请检查 safetensors 文件是否完整",
                model_path.display(),
                e
            ))
        })?;

        let model = BertModel::load(vb, &config).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "BERT 模型构建失败: {} — 可能权重文件与 config.json 不匹配",
                e
            ))
        })?;

        tracing::info!(path = %model_path.display(), "BERT 模型权重已加载");

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
                "BERT 分词器加载失败: {} — {}",
                tokenizer_path.display(),
                e
            ))
        })?;

        tracing::info!(
            path = %tokenizer_path.display(),
            vocab_size = tokenizer.get_vocab_size(true),
            "BERT 分词器已加载"
        );

        Ok(Self {
            model,
            tokenizer,
            dimension,
            device: device.clone(),
        })
    }

    /// 返回向量维度。
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// 对单条文本执行嵌入。
    ///
    /// 完整管线:
    /// 1. Tokenize: text → input_ids, attention_mask, token_type_ids
    /// 2. Forward: → last_hidden_state [1, seq_len, hidden_size]
    /// 3. Mean Pooling: attention_mask 加权平均
    /// 4. L2 Normalize: 归一化到单位向量
    ///
    /// 参数:
    /// - `text`: 待嵌入的文本。
    ///
    /// 返回:
    /// - L2 归一化后的 f32 向量。
    pub fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(ramaria_core::error::RamariaError::embedding(
                "嵌入文本不能为空",
            ));
        }

        // Step 1: Tokenize
        let encoding = self.tokenizer.encode(text, false).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("BERT 分词失败: {}", e))
        })?;

        let mut token_ids: Vec<u32> = encoding.get_ids().to_vec();
        let mut attention_mask: Vec<u32> = encoding.get_attention_mask().to_vec();
        let seq_len = pooling::truncate_to_max_seq_len(&mut token_ids, &mut attention_mask);

        // Step 2: 构建 candle 输入张量
        let input_ids = Tensor::new(token_ids.as_slice(), &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT input_ids 张量构建失败: {}",
                    e
                ))
            })?
            .unsqueeze(0)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT input_ids unsqueeze 失败: {}",
                    e
                ))
            })?;

        let attention_mask_tensor = Tensor::new(attention_mask.as_slice(), &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT attention_mask 张量构建失败: {}",
                    e
                ))
            })?
            .unsqueeze(0)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT attention_mask unsqueeze 失败: {}",
                    e
                ))
            })?;

        let token_type_ids =
            Tensor::zeros((1, seq_len), DType::U32, &self.device).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT token_type_ids 构建失败: {}",
                    e
                ))
            })?;

        // Step 3: Forward pass
        let hidden_states = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask_tensor))
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("BERT 前向推理失败: {}", e))
            })?;

        // Step 4: Mean Pooling（attention_mask 加权）
        // mean_pooling 返回 [1, hidden_size]（保留了 batch 维度），需 squeeze 为 [hidden_size]
        let pooled = pooling::mean_pooling(&hidden_states, &attention_mask_tensor, &self.device)?;
        let pooled_1d = pooled.squeeze(0).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("BERT pooled squeeze 失败: {}", e))
        })?;
        // pooled_1d: [hidden_size]

        // Step 5: L2 Normalize
        let normalized = pooling::l2_normalize(&pooled_1d, &self.device)?;

        // Step 6: 转为 Vec<f32>
        let vec = normalized.to_vec1::<f32>().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("BERT 输出向量提取失败: {}", e))
        })?;

        Ok(vec)
    }

    /// 对多条文本批量嵌入。
    ///
    /// 参数:
    /// - `texts`: 待嵌入文本列表。
    ///
    /// 返回:
    /// - 与输入顺序一致的向量列表。
    pub fn embed_batch_texts(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // 对每条文本分别 tokenize
        let mut token_id_vecs: Vec<Vec<u32>> = Vec::with_capacity(texts.len());
        let mut attention_vecs: Vec<Vec<u32>> = Vec::with_capacity(texts.len());

        for text in texts {
            let encoding = self.tokenizer.encode(*text, false).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("BERT 批量分词失败: {}", e))
            })?;
            let mut token_ids = encoding.get_ids().to_vec();
            let mut attention_mask = encoding.get_attention_mask().to_vec();
            pooling::truncate_to_max_seq_len(&mut token_ids, &mut attention_mask);
            token_id_vecs.push(token_ids);
            attention_vecs.push(attention_mask);
        }

        // Padding 到批次内最长序列
        let max_len = token_id_vecs.iter().map(|v| v.len()).max().unwrap_or(1);
        let batch_size = texts.len();

        let mut input_ids_flat = Vec::with_capacity(batch_size * max_len);
        let mut attention_flat = Vec::with_capacity(batch_size * max_len);

        for i in 0..batch_size {
            let len = token_id_vecs[i].len();
            for j in 0..max_len {
                if j < len {
                    input_ids_flat.push(token_id_vecs[i][j]);
                    attention_flat.push(attention_vecs[i][j]);
                } else {
                    input_ids_flat.push(0u32);
                    attention_flat.push(0u32);
                }
            }
        }

        // 构建批量张量
        let input_ids = Tensor::new(input_ids_flat.as_slice(), &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量 input_ids 失败: {}",
                    e
                ))
            })?
            .reshape(&[batch_size, max_len])
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量 reshape 失败: {}",
                    e
                ))
            })?;

        let attention_mask_tensor = Tensor::new(attention_flat.as_slice(), &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("BERT 批量 mask 失败: {}", e))
            })?
            .reshape(&[batch_size, max_len])
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量 mask reshape 失败: {}",
                    e
                ))
            })?;

        let token_type_ids = Tensor::zeros((batch_size, max_len), DType::U32, &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量 type_ids 失败: {}",
                    e
                ))
            })?;

        // 批量前向
        let hidden_states = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask_tensor))
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量前向推理失败: {}",
                    e
                ))
            })?;

        // 逐条 Mean Pooling + L2 Norm
        let dims = hidden_states.dims3().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("BERT 输出维度获取失败: {}", e))
        })?;
        let hidden_size = dims.2;
        let mut all_vectors = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            let actual_len = token_id_vecs[i].len();
            if actual_len == 0 {
                all_vectors.push(vec![0.0f32; hidden_size]);
                continue;
            }

            // 提取第 i 条样本
            let single = hidden_states.i(i).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("BERT 样本提取失败: {}", e))
            })?;
            // single: [seq_len, hidden_size]

            // 收集实际 mask
            let mask_vec: Vec<f32> = attention_vecs[i].iter().map(|&m| m as f32).collect();
            let mask_tensor = Tensor::new(mask_vec.as_slice(), &self.device).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("BERT mask 构建失败: {}", e))
            })?;

            let pooled =
                pooling::mean_pooling_single(&single, &mask_tensor, actual_len, &self.device)?;
            let normalized = pooling::l2_normalize(&pooled, &self.device)?;
            let vec = normalized.to_vec1::<f32>().map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "BERT 批量输出提取失败: {}",
                    e
                ))
            })?;
            all_vectors.push(vec);
        }

        Ok(all_vectors)
    }
}

// =========================================================
// 单元测试（内存构造小型模型，无需模型文件）
// =========================================================

#[cfg(test)]
mod tests;
