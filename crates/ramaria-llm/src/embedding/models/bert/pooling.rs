//! crates/ramaria-llm/src/embedding/models/bert/pooling.rs - BERT 输入裁剪与池化
//!
//! 设计特点:
//! - `truncate_to_max_seq_len`: token 序列与注意力掩码同步裁剪到模型最大长度
//! - `mean_pooling` / `mean_pooling_single`: attention_mask 加权平均，分别用于整批与批内单条
//! - `l2_normalize`: 委托共享工具 `common::l2_normalize`

use candle_core::{DType, Device, Tensor};
use ramaria_core::error::RamariaResult;

use super::MAX_SEQ_LEN;

// ---- 输入裁剪 ----

/// 将分词结果裁剪到模型最大序列长度，返回有效长度。
///
/// 参数:
/// - `token_ids`: token id 序列（原地截断）。
/// - `attention_mask`: 与 token 序列对齐的注意力掩码（原地截断）。
///
/// 返回:
/// - 裁剪后的有效 token 数（不大于 `MAX_SEQ_LEN`）。
///
/// 说明:
/// - tokenizer 不保证按模型配置截断，超长输入会在位置编码处失败，因此在构建张量前统一裁剪。
pub(super) fn truncate_to_max_seq_len(
    token_ids: &mut Vec<u32>,
    attention_mask: &mut Vec<u32>,
) -> usize {
    let raw_len = token_ids.len();
    let effective_len = raw_len.min(MAX_SEQ_LEN);
    token_ids.truncate(effective_len);
    attention_mask.truncate(effective_len);
    if raw_len > MAX_SEQ_LEN {
        tracing::debug!(
            raw_len,
            effective_len,
            max = MAX_SEQ_LEN,
            "BERT 输入序列超长，已按最大长度截断"
        );
    }
    effective_len
}

// ---- 池化辅助 ----

/// Mean pooling: attention_mask 加权平均。
///
/// hidden_states: [1, seq_len, hidden_size]
/// attention_mask: [1, seq_len]
pub(super) fn mean_pooling(
    hidden: &Tensor,
    mask: &Tensor,
    _device: &Device,
) -> RamariaResult<Tensor> {
    let mask_f32 = mask.to_dtype(DType::F32).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("mask 类型转换失败: {}", e))
    })?;
    // mask_f32: [1, seq_len]

    let mask_expanded = mask_f32.unsqueeze(2).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("mask expand 失败: {}", e))
    })?;
    // mask_expanded: [1, seq_len, 1]

    // candle 的 mul 要求形状完全一致，mask 是 [1, seq_len, 1] 而 hidden 是 [1, seq_len, H]，
    // 需使用 broadcast_mul 让 mask 的最后一维广播到 H。
    let masked = hidden.broadcast_mul(&mask_expanded).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("masked broadcast_mul 失败: {}", e))
    })?;
    // masked: [1, seq_len, hidden_size]

    let summed = masked
        .sum(1)
        .map_err(|e| ramaria_core::error::RamariaError::embedding(format!("sum 失败: {}", e)))?;
    // summed: [1, hidden_size]

    let mask_sum = mask_f32.sum_all().map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("mask_sum 失败: {}", e))
    })?;
    // mask_sum: scalar

    if mask_sum.to_scalar::<f32>().map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("scalar 提取失败: {}", e))
    })? < 1e-8
    {
        return Err(ramaria_core::error::RamariaError::embedding(
            "attention_mask 全为零，无法池化",
        ));
    }

    // summed: [1, H], mask_sum: scalar [] → 需 broadcast_div 广播标量
    let pooled = summed.broadcast_div(&mask_sum).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("broadcast_div 失败: {}", e))
    })?;

    Ok(pooled)
}

/// 单条 Mean pooling（批量处理时逐条调用）。
///
/// 参数:
/// - `hidden`: 批内单条 hidden state，形状 `[seq_len, hidden_size]`。
/// - `mask`: 注意力掩码 `[seq_len]`（f32），长度必须与 `hidden` 的第 0 维一致；
///   批量场景下须按整批 padding 后的长度对齐，padding 位置取 0。
/// - `device`: 计算设备。
///
/// 返回:
/// - 池化后的 `[hidden_size]` 向量（未归一化）。
pub(super) fn mean_pooling_single(
    hidden: &Tensor,
    mask: &Tensor,
    device: &Device,
) -> RamariaResult<Tensor> {
    // 添加 batch 维度
    let hidden_3d = hidden.unsqueeze(0).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("unsqueeze 失败: {}", e))
    })?;
    let mask_2d = mask.unsqueeze(0).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("mask unsqueeze 失败: {}", e))
    })?;

    let pooled_2d = mean_pooling(&hidden_3d, &mask_2d, device)?;
    // pooled_2d: [1, hidden_size]

    let pooled_1d = pooled_2d.squeeze(0).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("squeeze 失败: {}", e))
    })?;
    // pooled_1d: [hidden_size]

    Ok(pooled_1d)
}

/// L2 归一化（委托给共享工具 `common::l2_normalize`）。
pub(super) fn l2_normalize(vector: &Tensor, device: &Device) -> RamariaResult<Tensor> {
    super::super::common::l2_normalize(vector, device)
}
