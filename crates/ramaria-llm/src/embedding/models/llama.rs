//! crates/ramaria-llm/src/embedding/models/llama.rs - LLaMA/Qwen3 编码器
//!
//! 设计特点:
//! - 基于 `candle-transformers` 的 `Llama` 模型，兼容 Qwen2/Qwen3 架构（RMSNorm、RoPE、GQA、SwiGLU）
//! - 仅用于嵌入提取（非生成），不维护 KV cache
//! - 池化策略: Last token pooling（取最后一个有效 token 的 hidden state）+ L2 归一化
//! - 所有计算在 CPU 上执行，保证 Send + Sync
//! - Qwen3 config.json 使用 `hidden_size` 字段（与 BERT 一致），非 LLaMA 的 `dim`
//!
//! 架构差异说明:
//! - Qwen3-Embedding 基于 Qwen3 架构（LLaMA 变体），使用 causal attention mask
//! - 与 BERT 不同：没有 token_type_ids 输入，分词器会添加特殊 token 前缀
//! - 推荐池化：取最后一个 token（EOS 或文本末尾）的 hidden state

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::llama::{Cache, Config as LlamaConfig, Llama, LlamaEosToks};
use ramaria_core::error::RamariaResult;
use std::path::Path;
use tokenizers::Tokenizer;

// =========================================================
// 常量
// =========================================================

/// LLaMA/Qwen 默认最大序列长度
const MAX_SEQ_LEN: usize = 2048;

/// 模型权重文件
const MODEL_FILE: &str = "model.safetensors";

/// 分词器文件
const TOKENIZER_FILE: &str = "tokenizer.json";

/// 配置文件
const CONFIG_FILE: &str = "config.json";

// =========================================================
// LlamaEncoder
// =========================================================

/// LLaMA/Qwen3 嵌入编码器。
///
/// 职责:
/// - 加载 LLaMA 架构模型权重（safetensors 格式）和分词器
/// - 执行 `text → tokenize → forward → last token pool → L2 norm` 管线
///
/// 字段:
/// - `model`: candle Llama 模型实例
/// - `tokenizer`: HuggingFace tokenizer（BPE 类型）
/// - `dimension`: 向量维度（与 hidden_size 一致）
/// - `config`: 模型配置（用于创建 Cache）
/// - `device`: 计算设备（固定为 CPU）
pub struct LlamaEncoder {
    model: Llama,
    tokenizer: Tokenizer,
    dimension: usize,
    config: LlamaConfig,
    device: Device,
}

impl LlamaEncoder {
    /// 从模型目录加载 LLaMA/Qwen3 编码器。
    ///
    /// 参数:
    /// - `model_dir`: 包含 config.json、model.safetensors、tokenizer.json 的目录。
    ///
    /// 返回:
    /// - 已加载并可用于推理的 LlamaEncoder。
    ///
    /// 说明:
    /// - Qwen3 config.json 使用 `hidden_size` 字段，LLaMA 使用 `dim`。
    ///   candle 的 LlamaConfig 会自动处理两者的映射。
    ///
    /// 错误场景:
    /// - 模型文件缺失或损坏。
    /// - config.json 与 safetensors 权重不匹配。
    /// - 分词器缺失或格式无效。
    /// - 模型过大导致 OOM（CPU 内存不足）。
    pub fn load(model_dir: &Path) -> RamariaResult<Self> {
        let device = Device::Cpu;

        // ---- 加载 config.json ----
        // 注意: candle 0.8 的 LlamaConfig 未实现 Deserialize，需手动解析 JSON
        let config_path = model_dir.join(CONFIG_FILE);
        let config = Self::parse_config(&config_path)?;

        let dimension = config.hidden_size;
        tracing::info!(
            hidden_size = dimension,
            num_layers = config.num_hidden_layers,
            num_heads = config.num_attention_heads,
            num_kv_heads = config.num_key_value_heads,
            "LLaMA/Qwen 配置已加载"
        );

        // ---- 加载 safetensors 权重 ----
        let model_path = model_dir.join(MODEL_FILE);
        if !model_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "模型权重文件缺失: {}",
                model_path.display()
            )));
        }

        // 先读取 safetensors 头部，检查 tensor 键名（诊断 + 前缀检测）
        let st_keys: Vec<String> = match Self::read_safetensors_keys(&model_path) {
            Ok(keys) => {
                let first_10: Vec<&str> = keys.iter().take(10).map(|s| s.as_str()).collect();
                tracing::info!(
                    path = %model_path.display(),
                    total_keys = keys.len(),
                    sample_keys = ?first_10,
                    "safetensors tensor 键名"
                );
                keys
            }
            Err(e) => {
                tracing::warn!(
                    path = %model_path.display(),
                    error = %e,
                    "无法读取 safetensors tensor 键名（非致命错误，candle 将继续加载）"
                );
                Vec::new()
            }
        };

        // SAFETY: from_mmaped_safetensors 通过内存映射读取本地模型文件。
        // 调用者需保证:
        // (1) 模型文件来自可信来源（HuggingFace 官方仓库，通过 ModelManager 下载）。
        // (2) 在 mmap 期间文件不会被外部修改（Ramaria 进程独占写权限）。
        // (3) tensor 数据类型为 F32（DType::F32），与 candle 期望一致。
        // (4) 文件大小可能很大（Qwen3-Embedding-0.6B ≈ 1.2GB），需确保系统内存充足。
        // 若文件被外部截断或修改，mmap 会触发 SIGBUS。此风险由用户承担。
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_path.as_path()], DType::F32, &device)
        }
        .map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 权重加载失败: {} — {}。\n\
                 提示: Qwen3-Embedding-0.6B 约 1.2GB，请确保内存充足",
                model_path.display(),
                e
            ))
        })?;

        // ---- 检测 tensor 键名前缀并适配 ----
        // candle 的 Llama::load 内置使用 "model." 前缀（如 model.embed_tokens.weight），
        // 但某些 HuggingFace 模型（如 Qwen3-Embedding-0.6B）的 safetensors 文件不带此前缀。
        // 通过 rename_f 透明地将 candle 的 "model.xxx" 查询映射到实际键名 "xxx"。
        let vb = Self::apply_prefix_adaptation(vb, &st_keys);

        let model = Llama::load(vb, &config).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 模型构建失败: {} — 可能权重与配置不匹配",
                e
            ))
        })?;

        tracing::info!(
            path = %model_path.display(),
            "LLaMA/Qwen 模型权重已加载"
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
                "LLaMA/Qwen 分词器加载失败: {} — {}",
                tokenizer_path.display(),
                e
            ))
        })?;

        tracing::info!(
            path = %tokenizer_path.display(),
            vocab_size = tokenizer.get_vocab_size(true),
            "LLaMA/Qwen 分词器已加载"
        );

        Ok(Self {
            model,
            tokenizer,
            dimension,
            config,
            device,
        })
    }

    /// 返回向量维度。
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// 检测 safetensors 键名前缀并适配 candle 的 `Llama::load`。
    ///
    /// candle 的 `Llama::load` 硬编码使用 `"model."` 前缀查找 tensor
    /// （如 `model.embed_tokens.weight`），但某些 HuggingFace 模型
    /// （如 Qwen3-Embedding-0.6B）的 safetensors 文件不带此前缀。
    ///
    /// 通过 `VarBuilder::rename_f` 透明映射：
    /// `model.embed_tokens.weight` → `embed_tokens.weight`
    ///
    /// 参数:
    /// - `vb`: 从 safetensors 文件创建的 VarBuilder。
    /// - `st_keys`: safetensors 中的 tensor 键名列表（用于检测前缀）。
    ///
    /// 返回:
    /// - 可能经过 rename 适配的 VarBuilder（若无适配需求则为原值）。
    fn apply_prefix_adaptation<'a>(vb: VarBuilder<'a>, st_keys: &[String]) -> VarBuilder<'a> {
        // 检查第一个有效键（跳过 __metadata__）是否以 "model." 开头
        let needs_rename = st_keys
            .iter()
            .find(|k| *k != "__metadata__")
            .map(|k| !k.starts_with("model."))
            .unwrap_or(false);

        if needs_rename {
            tracing::info!("safetensors 键名无 'model.' 前缀，启用 VarBuilder rename_f 适配");
            vb.rename_f(|name: &str| {
                // candle 的 Llama::load 会添加 "model." 前缀，
                // 此处将其剥离以匹配实际 safetensors 键名。
                if let Some(stripped) = name.strip_prefix("model.") {
                    stripped.to_string()
                } else {
                    name.to_string()
                }
            })
        } else {
            tracing::debug!("safetensors 键名已有 'model.' 前缀，无需适配");
            vb
        }
    }

    /// 手动解析 config.json 并构造 candle `LlamaConfig`。
    ///
    /// candle 0.8 的 `LlamaConfig` 未实现 `Deserialize`，
    /// 因此需从原始 JSON 逐个提取字段后手工构造。
    ///
    /// 参数:
    /// - `config_path`: config.json 文件路径。
    ///
    /// 返回:
    /// - 构造好的 `LlamaConfig`。
    ///
    /// 说明:
    /// - Qwen3 的 config.json 使用 `hidden_size` 字段（与 BERT 一致），
    ///   而 LLaMA 标准使用 `dim`。此处优先读取 `hidden_size`，
    ///   若不存在则回退到 `dim`。
    fn parse_config(config_path: &Path) -> RamariaResult<LlamaConfig> {
        use super::common::{config_bool, config_f32, config_f64, config_u32_opt, config_usize};

        let file = std::fs::File::open(config_path).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 配置文件打开失败: {} — {}",
                config_path.display(),
                e
            ))
        })?;

        let raw: serde_json::Value = serde_json::from_reader(file).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 配置 JSON 解析失败: {} — {}",
                config_path.display(),
                e
            ))
        })?;

        // Qwen3 使用 hidden_size，LLaMA 标准使用 dim
        let hidden_size = if let Some(v) = raw.get("hidden_size").and_then(|v| v.as_u64()) {
            v as usize
        } else {
            config_usize(&raw, "dim", 1024)
        };

        let intermediate_size = config_usize(&raw, "intermediate_size", hidden_size * 8 / 3);

        // eos_token_id: Qwen3 通常是单个数字，LLaMA 可能是数组
        let eos_token_id = raw.get("eos_token_id").and_then(|v| {
            if let Some(n) = v.as_u64() {
                Some(LlamaEosToks::Single(n as u32))
            } else if let Some(arr) = v.as_array() {
                let ids: Vec<u32> = arr
                    .iter()
                    .filter_map(|x| x.as_u64().map(|n| n as u32))
                    .collect();
                if ids.is_empty() {
                    None
                } else {
                    Some(LlamaEosToks::Multiple(ids))
                }
            } else {
                None
            }
        });

        // rope_scaling: 默认为 None（Qwen3-Embedding 通常不带）
        let rope_scaling = None;

        Ok(LlamaConfig {
            hidden_size,
            intermediate_size,
            vocab_size: config_usize(&raw, "vocab_size", 151936),
            num_hidden_layers: config_usize(&raw, "num_hidden_layers", 28),
            num_attention_heads: config_usize(&raw, "num_attention_heads", 16),
            num_key_value_heads: config_usize(&raw, "num_key_value_heads", 8),
            use_flash_attn: config_bool(&raw, "use_flash_attn", false),
            rms_norm_eps: config_f64(&raw, "rms_norm_eps", 1e-6),
            rope_theta: config_f32(&raw, "rope_theta", 1_000_000.0),
            bos_token_id: config_u32_opt(&raw, "bos_token_id"),
            eos_token_id,
            rope_scaling,
            max_position_embeddings: config_usize(&raw, "max_position_embeddings", 32768),
            tie_word_embeddings: config_bool(&raw, "tie_word_embeddings", false),
        })
    }

    /// 读取 safetensors 文件中所有的 tensor 键名。
    ///
    /// 委托给共享工具 `common::read_safetensors_header` 读取 header 原始字节，
    /// 然后解析 JSON object 提取键名列表。只读 header（通常 < 100KB），不加载权重。
    fn read_safetensors_keys(model_path: &Path) -> RamariaResult<Vec<String>> {
        let header_bytes = super::common::read_safetensors_header(model_path)?;

        // header 是一个 JSON object，key 为 tensor 名称
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "safetensors header JSON 解析失败: {}",
                e
            ))
        })?;

        let keys: Vec<String> = header
            .as_object()
            .ok_or_else(|| {
                ramaria_core::error::RamariaError::embedding("safetensors header 不是 JSON object")
            })?
            .keys()
            .cloned()
            .collect();

        Ok(keys)
    }

    /// 对单条文本执行嵌入。
    ///
    /// 完整管线:
    /// 1. Tokenize: text → input_ids（含特殊 token）
    /// 2. Forward: → hidden_states [1, seq_len, hidden_size]
    /// 3. Last Token Pooling: 取序列中最后一个有效 token 的 hidden state
    /// 4. L2 Normalize
    ///
    /// 参数:
    /// - `text`: 待嵌入文本。
    ///
    /// 返回:
    /// - L2 归一化后的 f32 向量。
    ///
    /// 说明:
    /// - Qwen3 tokenizer 会自动添加 BOS/EOS token
    /// - 池化策略为 last token（取 attention_mask 中最后一个 1 对应的位置）
    pub fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(ramaria_core::error::RamariaError::embedding(
                "嵌入文本不能为空",
            ));
        }

        // Step 1: Tokenize
        let encoding = self.tokenizer.encode(text, true).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 分词失败: {} — 文本: '{}...'",
                e,
                &text[..text.len().min(60)]
            ))
        })?;

        let token_ids: Vec<u32> = encoding.get_ids().to_vec();
        let attention_mask: Vec<u32> = encoding.get_attention_mask().to_vec();
        let seq_len = token_ids.len();

        if seq_len > MAX_SEQ_LEN {
            tracing::warn!(
                seq_len,
                max = MAX_SEQ_LEN,
                "LLaMA/Qwen: 输入序列超长，将截断"
            );
        }

        let effective_len = seq_len.min(MAX_SEQ_LEN);
        let token_ids = &token_ids[..effective_len];
        let attention_mask = &attention_mask[..effective_len];

        // Step 2: 构建输入张量 [1, effective_len]
        let input_ids = Tensor::new(token_ids, &self.device)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "LLaMA input_ids 构建失败: {}",
                    e
                ))
            })?
            .unsqueeze(0)
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!("LLaMA unsqueeze 失败: {}", e))
            })?;

        // Step 3: Forward pass（start_pos=0 表示首次输入；不使用 KV cache，仅用于嵌入提取）
        let mut cache = Cache::new(true, DType::F32, &self.config, &self.device).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("LLaMA Cache 创建失败: {}", e))
        })?;
        let hidden_states = self.model.forward(&input_ids, 0, &mut cache).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 前向推理失败: {}。文本: '{}...'",
                e,
                &text[..text.len().min(60)]
            ))
        })?;
        // hidden_states: [1, effective_len, hidden_size]

        // Step 4: Last Token Pooling
        let pooled = Self::last_token_pooling(&hidden_states, attention_mask, &self.device)?;

        // Step 5: L2 Normalize
        let normalized = Self::l2_normalize(&pooled, &self.device)?;

        // Step 6: 转为 Vec<f32>
        let vec = normalized.to_vec1::<f32>().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "LLaMA/Qwen 输出向量提取失败: {}",
                e
            ))
        })?;

        Ok(vec)
    }

    /// 批量嵌入（逐条处理，因为 LLaMA 架构不支持变长 batch 前向）。
    ///
    /// 说明:
    /// - LLaMA 是 causal decoder，batch 前向需要统一序列长度
    /// - 当前实现逐条处理以保证正确性，后续可优化为 batch padding
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
    /// attention_mask: [seq_len] (u32 slice)
    ///
    /// 策略:
    /// - 从序列末尾向前扫描，找到第一个 mask=1 的位置
    /// - 取该位置的 hidden state 作为句子表示
    /// - 如果 mask 全为零（理论上不应发生），回退到取最后一个位置
    fn last_token_pooling(
        hidden: &Tensor,
        attention_mask: &[u32],
        _device: &Device,
    ) -> RamariaResult<Tensor> {
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

        // 提取 last_valid 位置的 hidden state: [1, hidden_size]
        let token_hidden = hidden.i((0, last_valid)).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "last token 提取失败 (pos={}): {}",
                last_valid, e
            ))
        })?;
        // token_hidden: [hidden_size]

        Ok(token_hidden)
    }

    /// L2 归一化（委托给共享工具 `common::l2_normalize`）。
    fn l2_normalize(vector: &Tensor, device: &Device) -> RamariaResult<Tensor> {
        super::common::l2_normalize(vector, device)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    // （原 empty_text_error 与 bert.rs:574 / llama_head_dim.rs 同名测试完全重复，
    //  仅验证 ramaria-core 的 error 构造，与 LlamaEncoder 逻辑无关，已删除）
}
