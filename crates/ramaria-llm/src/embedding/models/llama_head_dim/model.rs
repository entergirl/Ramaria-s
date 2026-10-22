//! crates/ramaria-llm/src/embedding/models/llama_head_dim/model.rs - 无状态 Qwen3 前向实现
//!
//! 设计特点:
//! - 参考 candle-transformers 0.10.2 `qwen3.rs` 实现，去除 KV cache 与 sliding window 分支
//! - 无状态推理不累积 K/V，也不依赖 candle 私有的 `clear_kv_cache`；`use_sliding_window=true` 时直接报错
//! - 权重键名与 candle `qwen3::Model` 一致（model.embed_tokens / model.layers.N.* / model.norm），前缀剥离由调用方完成
//! - 只做前向与因果掩码构造，不含分词与权重加载

use candle_core::{DType, Device, Tensor};
use candle_nn::{Activation, Embedding, Linear, Module, RmsNorm, VarBuilder};
use candle_transformers::models::qwen3::Config as Qwen3Config;
use candle_transformers::utils::repeat_kv;

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
pub(super) struct Qwen3EmbedModel {
    embed_tokens: Embedding,
    layers: Vec<DecoderLayer>,
    norm: RmsNorm,
    device: Device,
    dtype: DType,
}

impl Qwen3EmbedModel {
    pub(super) fn new(cfg: &Qwen3Config, vb: VarBuilder) -> candle_core::Result<Self> {
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
    pub(super) fn forward(&self, input: &Tensor) -> candle_core::Result<Tensor> {
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
