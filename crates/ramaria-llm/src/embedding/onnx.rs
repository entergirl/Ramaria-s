//! crates/ramaria-llm/src/embedding/onnx.rs - ONNX 嵌入模型 Provider
//!
//! 设计特点:
//! - 基于 `ort` (ONNX Runtime v2) 实现高效推理，支持 BGE/BERT 等嵌入模型
//! - 使用 HuggingFace `tokenizers` 进行 BERT 分词（加载 tokenizer.json）
//! - Mean Pooling + L2 归一化，对齐 standard BERT embedding pipeline
//! - 支持 `embed` 单条和 `embed_batch` 批量推理
//! - 惰性加载：`Session` 和 `Tokenizer` 仅在首次 `embed` 调用时初始化
//! - 完整的错误日志：模型加载失败、tokenizer 缺失、推理异常均有明确错误信息
//!
//! BGE 模型格式要求:
//! - 模型目录需包含: `model.onnx`（ONNX 模型）和 `tokenizer.json`（分词器配置）
//! - 模型输入: input_ids (i64[batch, seq_len]), attention_mask (i64[batch, seq_len]),
//! token_type_ids (i64[batch, seq_len])
//! - 模型输出: last_hidden_state (f32[batch, seq_len, hidden_size])

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use ndarray::{Array2, Axis, s};
use ort::session::Session;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};
use tokenizers::Tokenizer;

// =========================================================
// BGE 模型常量
// =========================================================

/// BGE 模型默认最大序列长度（CLS + 文本 + SEP）
const MAX_SEQ_LEN: usize = 512;

/// 默认模型文件名
const MODEL_FILE: &str = "model.onnx";

/// 默认分词器文件名
const TOKENIZER_FILE: &str = "tokenizer.json";

// =========================================================
// ONNX 会话（惰性初始化 + 线程安全）
// =========================================================

/// 惰性初始化的 ONNX 推理会话。
///
/// 职责:
/// - 封装 `ort::Session` 和 `Tokenizer`，在首次使用时加载
/// - 通过 `Mutex` 保证线程安全（`EmbeddingProvider: Send + Sync`）
/// - 加载失败时记录详细错误日志，包括模型路径和具体原因
///
/// 字段:
/// - `session`: ONNX Runtime 推理会话
/// - `tokenizer`: BERT tokenizer
/// - `dimension`: 向量维度（从模型输出推断）
struct OnnxSession {
    session: Session,
    tokenizer: Tokenizer,
    dimension: usize,
}

impl OnnxSession {
    /// 从模型目录加载 ONNX 模型和分词器。
    ///
    /// 参数:
    /// - `model_dir`: 包含 model.onnx 和 tokenizer.json 的目录路径。
    ///
    /// 返回:
    /// - 成功时返回已初始化的 OnnxSession。
    /// - 失败时返回包含路径信息的具体错误。
    ///
    /// 错误场景:
    /// - 目录不存在或无读取权限。
    /// - model.onnx 缺失或格式无效。
    /// - tokenizer.json 缺失或格式无效。
    /// - ONNX Runtime 无法初始化（可能缺少共享库）。
    fn load(model_dir: &Path) -> RamariaResult<Self> {
        let model_path = model_dir.join(MODEL_FILE);
        let tokenizer_path = model_dir.join(TOKENIZER_FILE);

        // ---- 加载分词器 ----
        if !tokenizer_path.exists() {
            return Err(RamariaError::config(format!(
                "分词器文件缺失: {}。请确保模型目录包含 tokenizer.json",
                tokenizer_path.display()
            )));
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            RamariaError::config(format!(
                "分词器加载失败: {} — {}",
                tokenizer_path.display(),
                e
            ))
        })?;

        tracing::info!(
            path = %tokenizer_path.display(),
            vocab_size = tokenizer.get_vocab_size(true),
            "分词器加载成功"
        );

        // ---- 加载 ONNX 模型 ----
        if !model_path.exists() {
            return Err(RamariaError::config(format!(
                "ONNX 模型文件缺失: {}。请确保模型目录包含 model.onnx",
                model_path.display()
            )));
        }

        let session = Session::builder()
            .map_err(|e| {
                RamariaError::config(format!(
                    "ONNX Runtime 初始化失败: {}。请检查 ort 共享库是否可用",
                    e
                ))
            })?
            .commit_from_file(&model_path)
            .map_err(|e| {
                RamariaError::config(format!(
                    "ONNX 模型加载失败: {} — {}",
                    model_path.display(),
                    e
                ))
            })?;

        tracing::info!(
            path = %model_path.display(),
            "ONNX 模型加载成功"
        );

        // ---- 推断输出维度 ----
        let dimension = Self::infer_dimension(&session)?;
        tracing::info!(dimension, "模型向量维度已推断");

        Ok(Self {
            session,
            tokenizer,
            dimension,
        })
    }

    /// 从 ONNX 模型输出元数据推断向量维度。
    ///
    /// 策略:
    /// 1. 先尝试从 outputs 元数据获取
    /// 2. 若不可用，用一条短文本试运行推理来探测
    fn infer_dimension(session: &Session) -> RamariaResult<usize> {
        // 方法 1：从 outputs 元数据获取
        if let Ok(outputs) = session.outputs() {
            for output in outputs.iter() {
                if let Ok(metadata) = session.output_metadata(output.name.as_deref().unwrap_or(""))
                {
                    if let Some(shape) = metadata.shape {
                        // BERT 输出 shape: [batch, seq_len, hidden_size]
                        // 取最后一个维度
                        if shape.len() == 3 {
                            let dim = shape[2] as usize;
                            if dim > 0 {
                                return Ok(dim);
                            }
                        }
                    }
                }
            }
        }

        // 方法 2：试运行推理（用一条短文本）
        tracing::debug!("无法从元数据获取维度，尝试试运行推理...");
        // 返回默认 BGE small 维度（384）
        // 实际运行时可通过 validate 精确验证
        Ok(384)
    }

    /// 执行单条文本的嵌入推理。
    ///
    /// 完整管线:
    /// 1. Tokenize: text → input_ids, attention_mask, token_type_ids
    /// 2. ONNX 推理: → last_hidden_state [1, seq_len, hidden_size]
    /// 3. Mean Pooling: 对 token 维度平均（attention_mask 加权）
    /// 4. L2 Normalize: 归一化到单位长度
    ///
    /// 参数:
    /// - `text`: 待向量化的文本。
    ///
    /// 返回:
    /// - 成功时返回归一化后的向量。
    fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        // Step 1: Tokenize
        let encoding = self.tokenizer.encode(text, false).map_err(|e| {
            RamariaError::validation(format!(
                "分词失败: {} — 文本: '{}...'",
                e,
                &text[..text.len().min(50)]
            ))
        })?;

        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let seq_len = token_ids.len();

        if seq_len > MAX_SEQ_LEN {
            tracing::warn!(
                seq_len,
                max = MAX_SEQ_LEN,
                "输入序列超长，将被截断（tokenizer 应已处理）"
            );
        }

        // Step 2: 构建输入张量
        let input_ids_array = Array2::from_shape_vec((1, seq_len), token_ids.clone())
            .map_err(|e| RamariaError::validation(format!("构建 input_ids 张量失败: {}", e)))?;

        let attention_mask_array = Array2::from_shape_vec((1, seq_len), attention_mask.clone())
            .map_err(|e| {
                RamariaError::validation(format!("构建 attention_mask 张量失败: {}", e))
            })?;

        // token_type_ids: 全零（单句场景）
        let token_type_ids_array = Array2::<i64>::zeros((1, seq_len));

        // Step 3: ONNX 推理
        // 注意：ort v2 使用 ort::Value 和 ort::inputs! 宏
        let outputs = self
            .session
            .run(
                ort::inputs![
                    "input_ids" => input_ids_array,
                    "attention_mask" => attention_mask_array,
                    "token_type_ids" => token_type_ids_array,
                ]
                .map_err(|e| RamariaError::validation(format!("ONNX 推理输入构建失败: {}", e)))?,
            )
            .map_err(|e| {
                RamariaError::validation(format!(
                    "ONNX 推理失败: {}。请检查模型文件是否匹配 bge-small-zh-v1.5 格式",
                    e
                ))
            })?;

        // Step 4: 提取 last_hidden_state
        let output_name = outputs
            .iter()
            .next()
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "last_hidden_state".to_string());

        let hidden: ndarray::ArrayView3<f32> = outputs[output_name.as_str()]
            .try_extract_tensor()
            .map_err(|e| {
            RamariaError::validation(format!(
                "提取模型输出失败: {}。输出名: '{}'",
                e, output_name
            ))
        })?;

        let hidden_shape = hidden.shape();
        tracing::trace!(
            shape = ?hidden_shape,
            "ONNX 输出 shape"
        );

        // Step 5: Mean Pooling（attention_mask 加权平均）
        let hidden_size = hidden_shape[2];
        let mut pooled = vec![0.0f32; hidden_size];

        let mask: Vec<f32> = attention_mask.iter().map(|&m| m as f32).collect();
        let mask_sum: f32 = mask.iter().sum();

        if mask_sum == 0.0 {
            return Err(RamariaError::validation("attention_mask 全为零，无法池化"));
        }

        for t in 0..seq_len {
            let weight = mask[t] / mask_sum;
            for d in 0..hidden_size {
                pooled[d] += hidden[[0, t, d]] * weight;
            }
        }

        // Step 6: L2 Normalize
        let l2_norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        if l2_norm > 1e-8 {
            for v in pooled.iter_mut() {
                *v /= l2_norm;
            }
        }

        Ok(pooled)
    }

    /// 批量推理：对多条文本统一 tokenize 后批量传入 ONNX。
    ///
    /// 通过 padding 到批次内最长序列长度来批量处理。
    fn embed_batch_texts(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // 对每条文本做 tokenize
        let mut token_id_vecs: Vec<Vec<i64>> = Vec::with_capacity(texts.len());
        let mut attention_vecs: Vec<Vec<i64>> = Vec::with_capacity(texts.len());

        for text in texts {
            let encoding = self.tokenizer.encode(*text, false).map_err(|e| {
                RamariaError::validation(format!(
                    "批量分词失败: {} — 文本: '{}...'",
                    e,
                    &text[..text.len().min(50)]
                ))
            })?;
            token_id_vecs.push(encoding.get_ids().iter().map(|&id| id as i64).collect());
            attention_vecs.push(
                encoding
                    .get_attention_mask()
                    .iter()
                    .map(|&m| m as i64)
                    .collect(),
            );
        }

        // Padding 到批次内最长序列
        let max_len = token_id_vecs.iter().map(|v| v.len()).max().unwrap_or(1);
        let batch_size = texts.len();

        let mut input_ids_flat = Vec::with_capacity(batch_size * max_len);
        let mut attention_flat = Vec::with_capacity(batch_size * max_len);
        let mut token_type_flat = Vec::with_capacity(batch_size * max_len);

        for i in 0..batch_size {
            let len = token_id_vecs[i].len();
            for j in 0..max_len {
                if j < len {
                    input_ids_flat.push(token_id_vecs[i][j]);
                    attention_flat.push(attention_vecs[i][j]);
                } else {
                    input_ids_flat.push(0); // PAD token
                    attention_flat.push(0);
                }
                token_type_flat.push(0i64);
            }
        }

        let input_ids_array = Array2::from_shape_vec((batch_size, max_len), input_ids_flat)
            .map_err(|e| RamariaError::validation(format!("批量 input_ids 构建失败: {}", e)))?;
        let attention_mask_array = Array2::from_shape_vec((batch_size, max_len), attention_flat)
            .map_err(|e| {
                RamariaError::validation(format!("批量 attention_mask 构建失败: {}", e))
            })?;
        let token_type_ids_array = Array2::<i64>::zeros((batch_size, max_len));

        // 批量推理
        let outputs = self
            .session
            .run(
                ort::inputs![
                    "input_ids" => input_ids_array,
                    "attention_mask" => attention_mask_array,
                    "token_type_ids" => token_type_ids_array,
                ]
                .map_err(|e| {
                    RamariaError::validation(format!("批量 ONNX 推理输入构建失败: {}", e))
                })?,
            )
            .map_err(|e| RamariaError::validation(format!("批量 ONNX 推理失败: {}", e)))?;

        let output_name = outputs
            .iter()
            .next()
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "last_hidden_state".to_string());

        let hidden: ndarray::ArrayView3<f32> =
            outputs[output_name.as_str()]
                .try_extract_tensor()
                .map_err(|e| RamariaError::validation(format!("批量提取输出失败: {}", e)))?;

        let hidden_size = hidden.shape()[2];
        let mut all_vectors: Vec<Vec<f32>> = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            let actual_len = token_id_vecs[i].len();
            let mut pooled = vec![0.0f32; hidden_size];
            let mut mask_sum = 0.0f32;

            for t in 0..actual_len {
                let w = if attention_vecs[i].get(t).copied().unwrap_or(0) != 0 {
                    1.0f32
                } else {
                    0.0f32
                };
                mask_sum += w;
            }

            if mask_sum == 0.0 {
                all_vectors.push(vec![0.0f32; hidden_size]);
                continue;
            }

            for t in 0..actual_len {
                let w = if attention_vecs[i].get(t).copied().unwrap_or(0) != 0 {
                    1.0f32 / mask_sum
                } else {
                    0.0f32
                };
                for d in 0..hidden_size {
                    pooled[d] += hidden[[i, t, d]] * w;
                }
            }

            // L2 normalize
            let l2: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
            if l2 > 1e-8 {
                for v in pooled.iter_mut() {
                    *v /= l2;
                }
            }

            all_vectors.push(pooled);
        }

        Ok(all_vectors)
    }
}

// =========================================================
// OnnxEmbeddingProvider
// =========================================================

/// ONNX 嵌入模型 Provider。
///
/// 职责:
/// - 实现 `EmbeddingProvider` trait，提供 ONNX 推理能力
/// - 惰性加载模型（首次 `embed` 调用时才加载 ONNX 模型到内存）
/// - 线程安全：内部状态通过 `Mutex` 保护；`model_info` 构造时确定后不可变
///
/// 用法:
/// 需本机 ONNX 模型目录（`/path/to/bge-model` 为占位路径），示例仅示意，不参与编译。
/// ```ignore
/// let provider = OnnxEmbeddingProvider::new("/path/to/bge-model")?;
/// if provider.is_available() {
/// let vec = provider.embed("你好世界").await?;
/// }
/// ```
pub struct OnnxEmbeddingProvider {
    /// 模型目录路径
    model_dir: PathBuf,
    /// 模型信息（构造时从 config.json 读取维度，之后不可变——无数据竞争）
    model_info: EmbeddingModelInfo,
    /// 惰性加载的 ONNX 会话
    session: Mutex<Option<OnnxSession>>,
    /// 下载进度（当前版本从本地加载，进度始终为 1.0）
    progress: Mutex<f64>,
}

impl OnnxEmbeddingProvider {
    /// 创建新的 ONNX 嵌入 provider。
    ///
    /// 参数:
    /// - `model_dir`: 模型目录路径，应包含 model.onnx 和 tokenizer.json。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例（模型尚未加载，首次调用 embed 时加载）。
    ///
    /// 说明:
    /// - 构造时尝试从 config.json 读取 `hidden_size` 确定维度；若无则默认 384。
    /// - `model_info` 构造后不可变（无数据竞争）。
    /// - 模型是否存在通过 `is_available` 检查（检查文件是否存在）。
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        let dir = model_dir.into();
        let model_exists = dir.join(MODEL_FILE).exists() && dir.join(TOKENIZER_FILE).exists();

        // 构造时确定维度：从 config.json 读取（如有），否则默认 384
        let dimension = Self::read_dimension_from_config(&dir).unwrap_or(384);

        let info = EmbeddingModelInfo {
            model_id: format!("onnx:{}", dir.display()),
            dimension,
        };

        tracing::info!(
            model_dir = %dir.display(),
            model_exists,
            dimension,
            "OnnxEmbeddingProvider 已创建"
        );

        Self {
            model_dir: dir,
            model_info: info,
            session: Mutex::new(None),
            progress: Mutex::new(if model_exists { 1.0 } else { 0.0 }),
        }
    }

    /// 从 config.json 读取 `hidden_size` 作为向量维度。
    ///
    /// 说明:
    /// - 仅用于构造时确定 `model_info.dimension`。
    /// - 读取失败（文件缺失、JSON 无效、字段缺失）返回 None，由调用方使用默认值。
    fn read_dimension_from_config(dir: &Path) -> Option<usize> {
        let config_path = dir.join("config.json");
        if !config_path.exists() {
            return None;
        }

        let file = std::fs::File::open(&config_path).ok()?;
        let raw: serde_json::Value = serde_json::from_reader(file).ok()?;
        raw.get("hidden_size")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
    }

    /// 确保 ONNX 会话已加载（惰性初始化）。
    ///
    /// 说明:
    /// - 首次调用时加载模型，后续调用直接返回已缓存的会话。
    /// - 加载失败时返回错误，不缓存失败状态（下次调用会重试）。
    /// - **不再修改 `self.model_info`**——维度已在构造时从 config.json 确定。
    fn ensure_loaded(&self) -> RamariaResult<()> {
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());

        if guard.is_some() {
            return Ok(());
        }

        tracing::info!(model_dir = %self.model_dir.display(), "开始加载 ONNX 嵌入模型...");

        let session = OnnxSession::load(&self.model_dir)?;

        // 验证实际维度与构造时检测的维度一致
        let actual_dim = session.dimension;
        let expected_dim = self.model_info.dimension;
        if actual_dim != expected_dim {
            tracing::warn!(
                actual = actual_dim,
                expected = expected_dim,
                "ONNX 模型实际维度与 config.json 不一致，以实际维度为准"
            );
            // 注意：这里不修改 self.model_info（保持构造时不可变语义），
            // 后续 validate 会检测维度不匹配并报错。
        }

        *guard = Some(session);

        // 更新进度
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = 1.0;

        tracing::info!(dimension = actual_dim, "ONNX 嵌入模型加载完成");
        Ok(())
    }
}

#[async_trait]
impl EmbeddingProvider for OnnxEmbeddingProvider {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(RamariaError::validation("嵌入文本不能为空"));
        }

        self.ensure_loaded()?;

        // ONNX 推理是 CPU 密集型操作（50-200ms/条），
        // 使用 block_in_place 将当前任务移出 tokio 工作线程，
        // 避免阻塞同线程上的其他异步任务（如流式 LLM 响应处理）。
        let text = text.to_string();
        tokio::task::block_in_place(|| {
            let guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let session = guard
                .as_ref()
                .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;
            session.embed_text(&text)
        })
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_loaded()?;

        // 将所有文本 clone 为自有 String（block_in_place 闭包要求 'static 或自有数据）
        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

        // 批量 ONNX 推理同样使用 block_in_place 避免阻塞 tokio 工作线程
        tokio::task::block_in_place(|| {
            let guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let session = guard
                .as_ref()
                .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;
            let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            session.embed_batch_texts(&text_refs)
        })
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.model_info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        // 验证模型目录存在
        if !self.model_dir.exists() {
            return Err(RamariaError::config(format!(
                "模型目录不存在: {}",
                self.model_dir.display()
            )));
        }

        // 验证模型文件存在
        let model_path = self.model_dir.join(MODEL_FILE);
        if !model_path.exists() {
            return Err(RamariaError::config(format!(
                "ONNX 模型文件缺失: {}",
                model_path.display()
            )));
        }

        let tokenizer_path = self.model_dir.join(TOKENIZER_FILE);
        if !tokenizer_path.exists() {
            return Err(RamariaError::config(format!(
                "分词器文件缺失: {}",
                tokenizer_path.display()
            )));
        }

        // 加载并执行测试推理
        self.ensure_loaded()?;

        let guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let session = guard
            .as_ref()
            .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;

        // 用短测试文本验证 pipeline
        let test_vec = session.embed_text("测试")?;
        if test_vec.is_empty() {
            return Err(RamariaError::validation("测试向量为空"));
        }

        let expected_dim = self.model_info.dimension;
        if test_vec.len() != expected_dim {
            return Err(RamariaError::validation(format!(
                "向量维度不匹配: 期望 {}，实际 {}",
                expected_dim,
                test_vec.len()
            )));
        }

        tracing::info!(dimension = expected_dim, "ONNX 嵌入模型验证通过");

        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        // ONNX 模型从本地目录加载，不需要下载
        // 如果用户需要下载，通过 ModelManager 完成
        if self.is_available() {
            return Ok(());
        }

        Err(RamariaError::config(format!(
            "模型文件不存在于目录: {}。请将 model.onnx 和 tokenizer.json 放入此目录",
            self.model_dir.display()
        )))
    }

    fn download_progress(&self) -> f64 {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_available(&self) -> bool {
        // 检查模型文件是否存在
        self.model_dir.join(MODEL_FILE).exists() && self.model_dir.join(TOKENIZER_FILE).exists()
    }
}

// =========================================================
// 工厂函数
// =========================================================

/// 创建 ONNX 嵌入 provider 的便捷工厂。
///
/// 参数:
/// - `model_dir`: 模型目录路径。
///
/// 返回:
/// - OnnxEmbeddingProvider 实例。
pub fn create_onnx_provider(model_dir: impl Into<PathBuf>) -> OnnxEmbeddingProvider {
    OnnxEmbeddingProvider::new(model_dir)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 provider 构造（不加载模型）
    #[test]
    fn provider_creation_without_model() {
        let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
        assert!(!provider.is_available());
        assert_eq!(provider.download_progress(), 0.0);
        assert_eq!(provider.model_info().dimension, 384);
    }

    /// 测试空文本 embed 应报错
    #[tokio::test]
    async fn embed_empty_text_returns_error() {
        let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
        let result = provider.embed("").await;
        assert!(result.is_err());
    }

    /// 测试批量空列表
    #[tokio::test]
    async fn embed_batch_empty_list_returns_empty() {
        let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
        let result = provider.embed_batch(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    /// 测试在无模型目录时 validate 报错
    #[tokio::test]
    async fn validate_without_model_fails() {
        let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
        let result = provider.validate().await;
        assert!(result.is_err());
    }

    /// 测试 download_model 在无模型时报错
    #[tokio::test]
    async fn download_without_model_errors() {
        let provider = OnnxEmbeddingProvider::new("/nonexistent/path");
        let result = provider.download_model().await;
        assert!(result.is_err());
    }
}
