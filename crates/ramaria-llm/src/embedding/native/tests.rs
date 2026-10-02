//! crates/ramaria-llm/src/embedding/native/tests.rs - 原生 safetensors 嵌入 Provider 单元测试
//!
//! 设计特点:
//! - 覆盖 provider 构造 / 空输入 / 无模型目录的 validate 与 download 行为
//! - 覆盖 model_info 默认维度、ID 格式与并发读取一致性
//! - 覆盖维度同步纯函数（更新 / 幂等 / 并发一致性）
//! - 不加载真实模型（构造路径仅探测文件与 config.json）

use super::*;

/// 测试 provider 构造（无模型文件）
#[test]
fn provider_creation_without_model() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    assert!(!provider.is_available());
    assert_eq!(provider.download_progress(), 0.0);
}

/// 测试空文本 embed 应报错
#[tokio::test]
async fn embed_empty_text_returns_error() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    let result = provider.embed("").await;
    assert!(result.is_err());
}

/// 测试批量空列表
#[tokio::test]
async fn embed_batch_empty_list_returns_empty() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    let result = provider.embed_batch(&[]).await.unwrap();
    assert!(result.is_empty());
}

/// 测试在无模型目录时 validate 报错
#[tokio::test]
async fn validate_without_model_fails() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    let result = provider.validate().await;
    assert!(result.is_err());
}

/// 测试 download_model 在无模型时报错
#[tokio::test]
async fn download_without_model_errors() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    let result = provider.download_model().await;
    assert!(result.is_err());
}

/// 测试 model_info 在未加载时返回默认维度
#[test]
fn model_info_default_dimension() {
    let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
    assert_eq!(provider.model_info().dimension, 384);
}

/// 测试 model_info 模型 ID 格式
#[test]
fn model_info_id_format() {
    let provider = NativeEmbeddingProvider::new("/test/model/dir").unwrap();
    assert!(provider.model_info().model_id.starts_with("native:"));
}

/// 并发调用 model_info()：验证 Mutex 化后的线程安全读取（无 panic、值一致）。
///
/// 说明:
/// - trait 签名按值返回后，读取路径全部走锁内 clone，无并发数据竞争
///   （维度同步的并发语义）。
/// - 模型加载的并发语义由 ensure_loaded 的 encoder 锁串行化保证；
///   此处与 sync_actual_dimension 纯函数测试共同覆盖"维度同步并发正确性"。
#[test]
fn model_info_concurrent_reads_are_consistent() {
    let provider = std::sync::Arc::new(NativeEmbeddingProvider::new("/test/model/dir").unwrap());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = std::sync::Arc::clone(&provider);
        handles.push(std::thread::spawn(move || {
            let info = p.model_info();
            assert_eq!(info.dimension, 384);
            assert!(info.model_id.starts_with("native:"));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

/// 维度同步纯函数：占位维度与真实维度不同时更新并返回变更标记。
#[test]
fn sync_actual_dimension_updates_when_different() {
    let mut stored = 384usize;
    let changed = sync_actual_dimension(&mut stored, 512);
    assert!(changed);
    assert_eq!(stored, 512);
}

/// 维度同步纯函数：真实维度与已存维度相同时不更新、返回未变更。
#[test]
fn sync_actual_dimension_noop_when_equal() {
    let mut stored = 512usize;
    let changed = sync_actual_dimension(&mut stored, 512);
    assert!(!changed);
    assert_eq!(stored, 512);
}

/// 维度同步纯函数：重复同步幂等，第二次起不再变更。
#[test]
fn sync_actual_dimension_is_idempotent() {
    let mut stored = 384usize;
    assert!(sync_actual_dimension(&mut stored, 768));
    assert!(!sync_actual_dimension(&mut stored, 768));
    assert_eq!(stored, 768);
}

/// 维度同步纯函数并发一致性：多个线程对同一共享维度槽做同步，结果确定且无 panic。
///
/// 说明:
/// - 模拟 ensure_loaded 并发完成后的同步：所有线程都以同一真实维度写入，
///   终值应为该维度；用 Mutex 串行化写，验证不引入数据竞争/悬挂。
#[test]
fn sync_actual_dimension_concurrent_is_consistent() {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(384usize));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = std::sync::Arc::clone(&slot);
        handles.push(std::thread::spawn(move || {
            let mut guard = s.lock().unwrap();
            sync_actual_dimension(&mut guard, 512);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*slot.lock().unwrap(), 512);
}
