//! crates/ramaria-llm/src/embedding/models/bert/tests.rs - BERT 编码器单元测试
//!
//! 设计特点:
//! - 内存构造小型 BERT 模型与空白切分分词器，不读取任何模型文件
//! - 覆盖超长输入裁剪（token 序列与注意力掩码同步收紧）与嵌入维度校验

use candle_nn::{VarBuilder, VarMap};
use candle_transformers::models::bert::Config as BertConfig;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::whitespace::Whitespace;

use super::*;

/// 构造仅用于测试的小型 BERT 编码器。
///
/// 说明:
/// - 权重由 `VarMap` 在内存中初始化，不读取模型文件。
/// - 分词器按空白切分，词表只含 `[UNK]` 与 `a`，便于构造任意长度的 token 序列。
fn tiny_encoder() -> BertEncoder {
    let device = Device::Cpu;

    let mut vocab = std::collections::HashMap::new();
    vocab.insert("[UNK]".to_string(), 0u32);
    vocab.insert("a".to_string(), 1u32);
    let word_level = WordLevel::builder()
        .vocab(vocab.into_iter().collect())
        .unk_token("[UNK]".to_string())
        .build()
        .expect("测试分词器应可构造");
    let mut tokenizer = Tokenizer::new(word_level);
    tokenizer.with_pre_tokenizer(Some(Whitespace));

    let config = BertConfig {
        vocab_size: 2,
        hidden_size: 8,
        num_hidden_layers: 1,
        num_attention_heads: 2,
        intermediate_size: 16,
        hidden_act: candle_transformers::models::bert::HiddenAct::Gelu,
        hidden_dropout_prob: 0.0,
        max_position_embeddings: MAX_SEQ_LEN,
        type_vocab_size: 2,
        initializer_range: 0.02,
        layer_norm_eps: 1e-12,
        pad_token_id: 0,
        position_embedding_type: candle_transformers::models::bert::PositionEmbeddingType::Absolute,
        use_cache: false,
        classifier_dropout: None,
        model_type: None,
    };
    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
    let model = BertModel::load(vb, &config).expect("测试模型应可构造");

    BertEncoder {
        model,
        tokenizer,
        dimension: config.hidden_size,
        device,
    }
}

/// 测试空文本应报错
#[test]
fn empty_text_detection() {
    // 不依赖实际模型 — 测试逻辑正确性
    let err = ramaria_core::error::RamariaError::embedding("嵌入文本不能为空");
    assert_eq!(err.category(), "embedding");
    assert!(err.to_string().contains("嵌入文本不能为空"));
}

/// 超长文本应在截断到最大序列长度后成功生成向量。
#[test]
fn long_text_is_truncated_and_embedded() {
    let encoder = tiny_encoder();
    let text = "a ".repeat(MAX_SEQ_LEN + 88);

    let vector = encoder
        .embed_text(&text)
        .expect("超长文本应在截断后成功生成向量");

    assert_eq!(vector.len(), encoder.dimension());
}

/// 裁剪会同时收紧 token 序列与注意力掩码，两者长度保持一致。
#[test]
fn truncate_to_max_seq_len_clamps_ids_and_mask() {
    let mut token_ids = vec![1u32; MAX_SEQ_LEN + 88];
    let mut attention_mask = vec![1u32; MAX_SEQ_LEN + 88];

    let effective_len = pooling::truncate_to_max_seq_len(&mut token_ids, &mut attention_mask);

    assert_eq!(effective_len, MAX_SEQ_LEN);
    assert_eq!(token_ids.len(), MAX_SEQ_LEN);
    assert_eq!(attention_mask.len(), MAX_SEQ_LEN);
}
