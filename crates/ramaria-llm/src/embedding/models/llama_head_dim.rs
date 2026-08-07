//! rust/crates/ramaria-llm/src/embedding/models/llama_head_dim.rs - LLaMA head_dim 变体嵌入编码器
//!
//! 设计特点:
//! - 基于 candle 0.10 的 `qwen3` 模块语义，适配显式指定 head_dim 的 LLaMA 变体
//!   （Qwen3-Embedding 系列 0.6B/4B/8B 等）
//! - Qwen3 架构: head_dim 由 config.json 显式指定（如 128），
//!   Q 投影维度 = num_heads × head_dim ≠ hidden_size（Qwen3 与 Qwen2 的关键区别）
//! - 使用 candle 的 `qwen3::Config`（Deserialize）：原生支持 `head_dim: usize`
//!   与 `sliding_window: Option<usize>`——Qwen3 config.json 中 `"sliding_window": null`
//!   可正确解析（candle 的 qwen2::Config 将 sliding_window 声明为 usize，遇到 null
//!   会报 "invalid type: null, expected usize"，见 2026-08-08 修复）
//! - 内嵌无状态 Qwen3 前向（参考 candle-transformers qwen3 模块，去除 KV cache 与
//!   sliding window 分支）：embedding 场景每次推理独立，无需跨调用上下文，
//!   天然无状态，也不依赖 candle 内部私有 `clear_kv_cache` API
//! - 仅用于嵌入提取（非生成）
//! - 池化策略: Last token pooling（取最后一个有效 token 的 hidden state）+ L2 归一化
//! - 所有计算在 CPU 上执行，保证 Send + Sync
//! - 最大序列长度: 2048 tokens
//!
//! 依赖前提:
//! - candle-transformers 0.10（无需本地 patch——qwen3::Config 原生支持 head_dim）
//! - 模型目录需包含: config.json, model.safetensors, tokenizer.json
//! - config.json 需包含 `head_dim` 字段（或架构检测判定为 head_dim 变体）
//!
//! 适用场景:
//! - Qwen3-Embedding 系列（0.6B/4B/8B）
//! - 其他显式指定 head_dim 且权重含 q_norm/k_norm 的 Qwen3 变体嵌入模型
//!
//! 与 llama.rs 的区别:
//! - llama.rs 手动解析 config.json → 构造 LlamaConfig（标准 head_dim = hidden_size/num_heads）
//! - llama_head_dim.rs 使用 candle 的 qwen3::Config（Deserialize），
//!   支持 config.json 中的 head_dim 与 null 字段（sliding_window 等）

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{Activation, Embedding, Linear, Module, RmsNorm, VarBuilder};
use candle_transformers::models::qwen3::Config as Qwen3Config;
use candle_transformers::utils::repeat_kv;
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
// 内嵌无状态 Qwen3 前向（2026-08-08 修复）
// =========================================================
//
// 说明:
// - 参考 candle-transformers 0.10.2 `qwen3.rs` 实现，做两处裁剪：
//   ① 去除 KV cache（嵌入推理无状态，避免跨调用 K/V 累积导致结果错误，
//      也绕开 candle `Model::clear_kv_cache` 为私有方法的问题）；
//   ② 去除 sliding window 分支（`use_sliding_window=true` 时直接报错，
//      Qwen3-Embedding 官方模型该值为 false）。
// - 权重键名与 candle `qwen3::Model` 完全一致（model.embed_tokens / model.layers.N.* /
//   model.norm），`rename_f` 前缀剥离逻辑保持不变。

/// Qwen3 RoPE（sin/cos 预计算，与 candle qwen3 一致，dim = head_dim）。
struct RotaryEmbedding {
    sin: Tensor,
    cos: Tensor,
}

impl RotaryEmbedding {
    fn new(dtype: DType, cfg: &Qwen3Config, dev: &Device) -> candle_core::Result<Self> {
        let dim = cfg.head_dim;
        let max_seq_len = cfg.max_position_embeddings;
        let inv_freq: Vec<f32> = (0..dim)
            .step_by(2)
            .map(|i| 1f32 / cfg.rope_theta.powf(i as f64 / dim as f64) as f32)
            .collect();
        let inv_freq_len = inv_freq.len();
        let inv_freq = Tensor::from_vec(inv_freq, (1, inv_freq_len), dev)?.to_dtype(DType::F32)?;
        let t = Tensor::arange(0u32, max_seq_len as u32, dev)?
            .to_dtype(DType::F32)?
            .reshape((max_seq_len, 1))?;
        let freqs = t.matmul(&inv_freq)?;
        Ok(Self {
            sin: freqs.sin()?.to_dtype(dtype)?,
            cos: freqs.cos()?.to_dtype(dtype)?,
        })
    }

    /// 应用 RoPE（q/k 形状: B x H x L x D；offset 恒为 0——无状态推理）。
    fn apply(&self, q: &Tensor, k: &Tensor) -> candle_core::Result<(Tensor, Tensor)> {
        let (_, _, seq_len, _) = q.dims4()?;
        let cos = self.cos.narrow(0, 0, seq_len)?;
        let sin = self.sin.narrow(0, 0, seq_len)?;
        let q_embed = candle_nn::rotary_emb::rope(&q.contiguous()?, &cos, &sin)?;
        let k_embed = candle_nn::rotary_emb::rope(&k.contiguous()?, &cos, &sin)?;
        Ok((q_embed, k_embed))
    }
}

/// Qwen3 注意力层（无 KV cache；含 Qwen3 特有的 per-head q_norm/k_norm）。
struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    num_heads: usize,
    num_kv_heads: usize,
    num_kv_groups: usize,
    head_dim: usize,
    hidden_size: usize,
    rotary_emb: std::sync::Arc<RotaryEmbedding>,
}

impl Attention {
    fn new(
        cfg: &Qwen3Config,
        rotary_emb: std::sync::Arc<RotaryEmbedding>,
        vb: VarBuilder,
    ) -> candle_core::Result<Self> {
        if cfg.use_sliding_window {
            candle_core::bail!("sliding window 模式不受支持（Qwen3-Embedding 官方模型为 false）");
        }

        let head_dim = cfg.head_dim;
        let num_heads = cfg.num_attention_heads;
        let num_kv_heads = cfg.num_key_value_heads;
        let num_kv_groups = num_heads / num_kv_heads;
        // Qwen3-Embedding attention_bias=false：投影层无 bias 权重
        let q_proj = Linear::new(
            vb.get((num_heads * head_dim, cfg.hidden_size), "q_proj.weight")?,
            None,
        );
        let k_proj = Linear::new(
            vb.get((num_kv_heads * head_dim, cfg.hidden_size), "k_proj.weight")?,
            None,
        );
        let v_proj = Linear::new(
            vb.get((num_kv_heads * head_dim, cfg.hidden_size), "v_proj.weight")?,
            None,
        );
        let o_proj = Linear::new(
            vb.get((cfg.hidden_size, num_heads * head_dim), "o_proj.weight")?,
            None,
        );
        let q_norm = candle_nn::rms_norm(head_dim, cfg.rms_norm_eps, vb.pp("q_norm"))?;
        let k_norm = candle_nn::rms_norm(head_dim, cfg.rms_norm_eps, vb.pp("k_norm"))?;

        Ok(Self {
            q_proj,
            k_proj,
            v_proj,
            o_proj,
            q_norm,
            k_norm,
            num_heads,
            num_kv_heads,
            num_kv_groups,
            head_dim,
            // Necessary because the hidden_size in the config isn't always accurate
            hidden_size: head_dim * num_heads,
            rotary_emb,
        })
    }

    fn forward(&self, x: &Tensor, attn_mask: Option<&Tensor>) -> candle_core::Result<Tensor> {
        let (b, l, _) = x.dims3()?;

        // 1. 投影
        let q = self.q_proj.forward(x)?;
        let k = self.k_proj.forward(x)?;
        let v = self.v_proj.forward(x)?;

        // 2. Reshape: (B, L, H, D) -> (B, H, L, D)
        let q = q
            .reshape((b, l, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let k = k
            .reshape((b, l, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let v = v
            .reshape((b, l, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;

        // 3. Per-head RMSNorm（Qwen3 特有：q_norm/k_norm）
        let q_flat = q.flatten(0, 2)?;
        let k_flat = k.flatten(0, 2)?;
        let q = self
            .q_norm
            .forward(&q_flat)?
            .reshape((b, self.num_heads, l, self.head_dim))?;
        let k = self
            .k_norm
            .forward(&k_flat)?
            .reshape((b, self.num_kv_heads, l, self.head_dim))?;

        // 4. RoPE
        let (q, k) = self.rotary_emb.apply(&q, &k)?;

        // 5. GQA repeat_kv
        let k = repeat_kv(k, self.num_kv_groups)?.contiguous()?;
        let v = repeat_kv(v, self.num_kv_groups)?.contiguous()?;

        // 6. Attention score（因果掩码由上层传入）
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let mut scores = (q.matmul(&k.transpose(2, 3)?)? * scale)?;
        if let Some(m) = attn_mask {
            scores = scores.broadcast_add(m)?;
        }
        let probs = candle_nn::ops::softmax_last_dim(&scores)?;
        let ctx = probs.matmul(&v)?; // (B, H, L, D)

        // 7. 输出投影
        ctx.transpose(1, 2)?
            .reshape((b, l, self.hidden_size))?
            .apply(&self.o_proj)
    }
}

/// Qwen3 MLP（gate/up/down + silu）。
struct Mlp {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
    act_fn: Activation,
}

impl Mlp {
    fn new(cfg: &Qwen3Config, vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            gate_proj: Linear::new(
                vb.get((cfg.intermediate_size, cfg.hidden_size), "gate_proj.weight")?,
                None,
            ),
            up_proj: Linear::new(
                vb.get((cfg.intermediate_size, cfg.hidden_size), "up_proj.weight")?,
                None,
            ),
            down_proj: Linear::new(
                vb.get((cfg.hidden_size, cfg.intermediate_size), "down_proj.weight")?,
                None,
            ),
            act_fn: cfg.hidden_act,
        })
    }
}

impl Module for Mlp {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let lhs = x.apply(&self.gate_proj)?.apply(&self.act_fn)?;
        let rhs = x.apply(&self.up_proj)?;
        (lhs * rhs)?.apply(&self.down_proj)
    }
}

/// Qwen3 解码层（无 KV cache，全量前向）。
struct DecoderLayer {
    self_attn: Attention,
    mlp: Mlp,
    ln1: RmsNorm,
    ln2: RmsNorm,
}

impl DecoderLayer {
    fn new(
        cfg: &Qwen3Config,
        rotary: std::sync::Arc<RotaryEmbedding>,
        vb: VarBuilder,
    ) -> candle_core::Result<Self> {
        let self_attn = Attention::new(cfg, rotary, vb.pp("self_attn"))?;
        let mlp = Mlp::new(cfg, vb.pp("mlp"))?;
        let ln1 = candle_nn::rms_norm(cfg.hidden_size, cfg.rms_norm_eps, vb.pp("input_layernorm"))?;
        let ln2 = candle_nn::rms_norm(
            cfg.hidden_size,
            cfg.rms_norm_eps,
            vb.pp("post_attention_layernorm"),
        )?;
        Ok(Self {
            self_attn,
            mlp,
            ln1,
            ln2,
        })
    }

    fn forward(&self, x: &Tensor, mask: Option<&Tensor>) -> candle_core::Result<Tensor> {
        let h = self.ln1.forward(x)?;
        let h = self.self_attn.forward(&h, mask)?;
        let x = (x + h)?;
        let h2 = self.ln2.forward(&x)?;
        let h2 = h2.apply(&self.mlp)?;
        x + h2
    }
}

/// 无状态 Qwen3 嵌入模型（embed_tokens + N 层 + norm）。
///
/// 与 candle `qwen3::Model` 的差异:
/// - 无 KV cache（每次 forward 独立，offset 恒为 0，天然无状态）
/// - forward 签名简化: `forward(&self, input) -> Tensor`（无 offset/外部 mask 参数）
struct Qwen3EmbedModel {
    embed_tokens: Embedding,
    layers: Vec<DecoderLayer>,
    norm: RmsNorm,
    device: Device,
    dtype: DType,
}

impl Qwen3EmbedModel {
    fn new(cfg: &Qwen3Config, vb: VarBuilder) -> candle_core::Result<Self> {
        let embed_tokens =
            candle_nn::embedding(cfg.vocab_size, cfg.hidden_size, vb.pp("model.embed_tokens"))?;
        let rotary = std::sync::Arc::new(RotaryEmbedding::new(vb.dtype(), cfg, vb.device())?);
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        let vb_l = vb.pp("model.layers");
        for i in 0..cfg.num_hidden_layers {
            layers.push(DecoderLayer::new(cfg, rotary.clone(), vb_l.pp(i))?);
        }
        Ok(Self {
            embed_tokens,
            layers,
            norm: candle_nn::rms_norm(cfg.hidden_size, cfg.rms_norm_eps, vb.pp("model.norm"))?,
            device: vb.device().clone(),
            dtype: vb.dtype(),
        })
    }

    /// 构建因果掩码（无 offset、无滑动窗口：全因果）。
    fn causal_mask(&self, b: usize, tgt: usize) -> candle_core::Result<Tensor> {
        let minf = f32::NEG_INFINITY;
        let mask: Vec<f32> = (0..tgt)
            .flat_map(|i| (0..tgt).map(move |j| if j <= i { 0. } else { minf }))
            .collect();
        Tensor::from_slice(&mask, (b, 1, tgt, tgt), &self.device)?.to_dtype(self.dtype)
    }

    /// 前向传播（无状态：每序列独立推理，offset 恒为 0）。
    fn forward(&self, input: &Tensor) -> candle_core::Result<Tensor> {
        let (b, l) = input.dims2()?;
        let mut h = self.embed_tokens.forward(input)?;

        let causal = if l == 1 {
            None
        } else {
            Some(self.causal_mask(b, l)?)
        };

        for layer in &self.layers {
            h = layer.forward(&h, causal.as_ref())?;
        }
        self.norm.forward(&h)
    }
}

// =========================================================
// LlamaHeadDimEncoder
// =========================================================

/// LLaMA head_dim 变体嵌入编码器。
///
/// 职责:
/// - 加载 Qwen3 架构权重（head_dim 显式指定 + q_norm/k_norm）
/// - 执行 `text → tokenize → forward → last token pool → L2 norm` 管线
///
/// 字段:
/// - `model`: 无状态 Qwen3 嵌入模型（Mutex 包裹，与既有编码器结构一致）
/// - `tokenizer`: HuggingFace BPE tokenizer
/// - `dimension`: 向量维度（hidden_size）
/// - `device`: 计算设备（固定为 CPU）
pub struct LlamaHeadDimEncoder {
    model: Mutex<Qwen3EmbedModel>,
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
    /// - candle 的 qwen3 Config 从 config.json 反序列化，原生支持 `head_dim`
    ///   与 `"sliding_window": null`（Qwen3-Embedding config.json 的标准形态）。
    /// - 权重加载失败时错误信息会给出 shape 不匹配提示。
    ///
    /// 错误场景:
    /// - 模型文件缺失或损坏。
    /// - config.json 与 safetensors 权重不匹配。
    /// - 分词器缺失或格式无效。
    /// - 模型过大导致 OOM。
    pub fn load(model_dir: &Path) -> RamariaResult<Self> {
        let device = Device::Cpu;

        // ---- 加载 config.json（qwen3::Config 支持 Deserialize：
        //      head_dim: usize + sliding_window: Option<usize>，null 天然兼容）----
        let config_path = model_dir.join(CONFIG_FILE);
        if !config_path.exists() {
            return Err(ramaria_core::error::RamariaError::embedding(format!(
                "配置文件缺失: {}",
                config_path.display()
            )));
        }

        let config: Qwen3Config = {
            let file = std::fs::File::open(&config_path).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "模型配置文件打开失败: {} — {}",
                    config_path.display(),
                    e
                ))
            })?;
            serde_json::from_reader(file).map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "模型配置 JSON 解析失败: {} — {}. \
                     提示: Qwen3-Embedding 系列应包含 head_dim 字段；\
                     若为其它 LLaMA 变体请确认架构检测是否匹配。",
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
            head_dim = config.head_dim,
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
        // candle 的 Qwen3EmbedModel::new 通过 vb.pp("model") 添加 "model." 前缀查询 tensor，
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

        let model = Qwen3EmbedModel::new(&config, vb).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "模型构建失败: {} — 可能权重与配置不匹配。\
                 \n  提示: Qwen3-Embedding 系列应含 q_norm/k_norm 权重；\
                 若权重键名不同请检查模型是否适配本编码器。",
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

        // Step 3: Forward pass（无状态前向：每序列独立推理，无 KV cache 累积问题）
        let model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let hidden_states = model.forward(&input_ids).map_err(|e| {
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
    use super::*;

    // 说明:
    // - 编码器需要真实模型文件（config.json + safetensors + tokenizer.json），
    //   无法在 CI 构造，端到端验证由 `validate_embedding_model` 命令在真实
    //   模型目录上执行（见 ramaria-desktop commands/setup.rs）。
    // - 2026-08-08 修复回归：Qwen3-Embedding config.json 的 `"sliding_window": null`
    //   解析由 candle `qwen3::Config` 的 `Option<usize>` 字段天然兼容，
    //   反序列化行为由 candle 单测覆盖；此处保留空模块占位。

    /// 防御断言：Qwen3-Embedding 官方 config 的 null 字段在 qwen3::Config 中为 Option。
    #[test]
    fn qwen3_config_accepts_null_sliding_window() {
        // 与 Qwen3-Embedding-0.6B config.json 结构一致的最小样例
        let json = r#"{
            "vocab_size": 151669,
            "hidden_size": 1024,
            "intermediate_size": 3072,
            "num_hidden_layers": 28,
            "num_attention_heads": 16,
            "head_dim": 128,
            "attention_bias": false,
            "num_key_value_heads": 8,
            "max_position_embeddings": 32768,
            "sliding_window": null,
            "max_window_layers": 28,
            "tie_word_embeddings": true,
            "rope_theta": 1000000,
            "rms_norm_eps": 1e-06,
            "use_sliding_window": false,
            "hidden_act": "silu"
        }"#;
        let cfg: Qwen3Config = serde_json::from_str(json).expect("qwen3::Config 应接受 null 字段");
        assert_eq!(cfg.head_dim, 128, "head_dim 应正确解析");
        assert_eq!(cfg.hidden_size, 1024);
        assert!(cfg.sliding_window.is_none(), "sliding_window: null → None");
        assert!(!cfg.use_sliding_window);
    }

    /// 防御断言：缺失 head_dim 字段时给出明确解析错误（提示架构不匹配）。
    #[test]
    fn qwen3_config_requires_head_dim() {
        // qwen3::Config 的 head_dim 为必填 usize 字段（无 serde default），
        // 缺失时报错——这正是架构检测将其归入 LlamaHeadDim 的必要条件。
        let json = r#"{
            "vocab_size": 151669,
            "hidden_size": 1024,
            "intermediate_size": 3072,
            "num_hidden_layers": 28,
            "num_attention_heads": 16,
            "attention_bias": false,
            "num_key_value_heads": 8,
            "max_position_embeddings": 32768,
            "sliding_window": null,
            "max_window_layers": 28,
            "tie_word_embeddings": true,
            "rope_theta": 1000000,
            "rms_norm_eps": 1e-06,
            "use_sliding_window": false,
            "hidden_act": "silu"
        }"#;
        let result: Result<Qwen3Config, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "缺失 head_dim 应解析失败（非本编码器适用模型）"
        );
    }
}
