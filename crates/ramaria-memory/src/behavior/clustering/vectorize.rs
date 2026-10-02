//! crates/ramaria-memory/src/behavior/clustering/vectorize.rs - 双通道向量化
//!
//! 设计特点:
//! - 反应通道文本 = paraphrase ⊕ attitude；情境通道文本 = 关键词空格拼接。
//! - 两通道各自批量向量化，批量失败回退逐条尝试。
//! - 单条失败只置该样本对应通道为 None，不整体失败（静默降级链）。
//! - embedding 不可用时全部通道留空，聚类退化为纯关键词 Jaccard。

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::MemoryEvent;
use std::collections::HashMap;

use super::sample::BehaviorSample;

// =========================================================
// 双通道向量化
// =========================================================

/// 双通道向量化：为样本填充情境通道与反应通道向量。
///
/// 参数:
/// - `samples`: 待向量化样本（原地修改）。
/// - `embedder`: 嵌入模型 provider；`None` 表示 embedding 不可用（纯关键词降级）。
///
/// 说明:
/// - 反应通道文本 = paraphrase ⊕ attitude；情境通道文本 = 关键词空格拼接。
/// - 单条 embedding 失败只影响该样本对应通道（记 warn 后置 None），不整体失败——
///   保证 embedding 局部故障时聚类仍可降级运行（静默降级链）。
pub async fn vectorize(
    samples: &mut [BehaviorSample],
    events: &[MemoryEvent],
    embedder: Option<&dyn EmbeddingProvider>,
) -> RamariaResult<()> {
    let Some(embedder) = embedder else {
        // embedding 不可用：全部通道留空，聚类退化为纯关键词 Jaccard（β=0）
        tracing::warn!("行为聚类 embedding 不可用，降级纯关键词 Jaccard 通道");
        return Ok(());
    };

    // 事件 id → 反应通道文本（paraphrase ⊕ attitude）
    let reaction_texts: HashMap<i64, Option<String>> = events
        .iter()
        .map(|e| {
            let text = match (&e.paraphrase, &e.attitude) {
                (Some(p), Some(a)) => Some(format!("{}\n{}", p, a)),
                (Some(p), None) => Some(p.clone()),
                (None, Some(a)) => Some(a.clone()),
                (None, None) => None,
            };
            (e.id, text)
        })
        .collect();

    // 收集需要向量化的文本（按样本顺序，保留索引）
    let mut reaction_payloads: Vec<Option<String>> = Vec::with_capacity(samples.len());
    let mut situation_payloads: Vec<Option<String>> = Vec::with_capacity(samples.len());
    for s in samples.iter() {
        let r = reaction_texts.get(&s.event_id).cloned().flatten();
        let s_text = if s.situation_keywords.is_empty() {
            None
        } else {
            Some(s.situation_keywords.join(" "))
        };
        reaction_payloads.push(r);
        situation_payloads.push(s_text);
    }

    // 分两批向量化（反应通道 + 情境通道），各自独立降级
    vectorize_channel(samples, events, embedder, &reaction_payloads, |s, v| {
        s.reaction_vector = Some(v);
    })
    .await?;
    vectorize_channel(samples, events, embedder, &situation_payloads, |s, v| {
        s.situation_vector = Some(v);
    })
    .await?;

    Ok(())
}

/// 对单通道批量向量化；缺失文本或单条失败 → 对应向量为 None（不阻塞整体）。
async fn vectorize_channel(
    samples: &mut [BehaviorSample],
    events: &[MemoryEvent],
    embedder: &dyn EmbeddingProvider,
    payloads: &[Option<String>],
    assign: impl Fn(&mut BehaviorSample, Vec<f32>),
) -> RamariaResult<()> {
    let _ = events;
    // 先批量请求可向量化文本
    let mut batch_texts: Vec<&str> = Vec::new();
    let mut batch_index: Vec<usize> = Vec::new(); // 样本索引
    for (i, p) in payloads.iter().enumerate() {
        if let Some(text) = p {
            batch_texts.push(text.as_str());
            batch_index.push(i);
        }
    }
    if batch_texts.is_empty() {
        return Ok(());
    }
    let vectors = match embedder.embed_batch(&batch_texts).await {
        Ok(v) => v,
        Err(e) => {
            // 批量失败：回退逐条尝试，单条失败记 warn 置 None（静默降级）
            tracing::warn!(error = %e, "行为聚类批量向量化失败，回退逐条");
            let mut out = Vec::with_capacity(batch_texts.len());
            for t in batch_texts {
                match embedder.embed(t).await {
                    Ok(v) => out.push(v),
                    Err(e2) => {
                        tracing::warn!(error = %e2, "行为聚类单条向量化失败，置 None");
                        out.push(Vec::new()); // 哨兵空向量
                    }
                }
            }
            out
        }
    };
    for (k, &sample_idx) in batch_index.iter().enumerate() {
        let v = &vectors[k];
        if !v.is_empty() {
            assign(&mut samples[sample_idx], v.clone());
        }
    }
    Ok(())
}
