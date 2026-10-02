//! crates/ramaria-core/src/traits/embedding.rs - Ramaria Embedding Provider 抽象模块
//!
//! 设计特点:
//! - 描述 embedding 模型标识、向量维度与单条嵌入结果
//! - 抽象模型下载、校验、进度查询与可用性判断
//! - 提供单条与批量文本向量化入口
//! - 供混合 RAG 的向量通道与首次配置向导复用
//! - 未完成下载或校验失败时可用性必须为 false

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::RamariaResult;

// =========================================================
// Embedding Provider 抽象层
// =========================================================

/// Embedding 模型信息。
///
/// 职责:
/// - 描述当前 embedding 模型的稳定标识和向量维度。
/// - 供配置向导、索引初始化和一致性检查使用。
#[derive(Debug, Clone)]
pub struct EmbeddingModelInfo {
    /// 模型标识
    pub model_id: String,
    /// 向量维度
    pub dimension: usize,
}

/// 单条嵌入结果。
///
/// 职责:
/// - 将业务对象 ID 与向量数据绑定。
/// - 供向量索引写入时使用。
#[derive(Debug, Clone)]
pub struct Embedding {
    /// 向量 ID
    pub id: Uuid,
    /// 向量数据
    pub vector: Vec<f32>,
}

/// Embedding Provider 抽象 trait。
///
/// 职责:
/// - 下载和校验 embedding 模型。
/// - 将文本转换为向量，供混合 RAG 的向量通道使用。
/// - 暴露下载进度和可用性，供首次配置向导展示。
///
/// 实现要求:
/// - 未完成下载或校验失败时，`is_available` 必须返回 false。
/// - `validate` 至少应执行一次测试向量生成。
/// - 不应在核心层直接依赖具体模型库。
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// 为单条文本生成嵌入向量。
    ///
    /// 参数:
    /// - `text`: 待向量化文本。
    ///
    /// 返回:
    /// - 成功时返回向量。
    /// - 模型不可用或生成失败时返回错误。
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>>;

    /// 为多条文本批量生成嵌入向量。
    ///
    /// 参数:
    /// - `texts`: 待向量化文本列表。
    ///
    /// 返回:
    /// - 与输入顺序一致的向量列表。
    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>>;

    /// 获取模型信息（按值返回，线程安全）。
    ///
    /// 说明:
    /// - 按值返回而非引用：实现内部可能用锁保护可变维度（如 native provider
    ///   在模型加载后同步实际维度），调用方可安全地在并发场景读取。
    /// - 结构为 `Clone`，每次调用复制 model_id 字符串（短字符串，开销可忽略）。
    fn model_info(&self) -> EmbeddingModelInfo;

    /// 验证模型可用。
    ///
    /// 检查内容:
    /// - 模型文件是否存在。
    /// - 单条测试文本是否能成功生成向量。
    async fn validate(&self) -> RamariaResult<()>;

    /// 下载模型。
    ///
    /// 说明:
    /// - 若模型已存在，实现可以直接返回成功。
    /// - 下载进度通过 `download_progress` 暴露。
    async fn download_model(&self) -> RamariaResult<()>;

    /// 返回下载进度 0.0..1.0。
    fn download_progress(&self) -> f64;

    /// 模型是否已下载且可用。
    fn is_available(&self) -> bool;
}
