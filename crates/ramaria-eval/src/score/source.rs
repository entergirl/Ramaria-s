//! crates/ramaria-eval/src/score/source.rs - Ramaria 评测单元证据解析器
//!
//! 设计特点:
//! - 先批量预取再同步查表：异步检索集中在 prefetch，指标计算保持纯同步
//! - 进程内缓存以 `(层名, 主键)` 为键，同一单元跨题只查一次库
//! - 解析失败与不可映射统一表现为查不到，由指标侧计入不可映射单元
//! - 只经服务层用例读取证据，不直连存储

use std::collections::HashMap;

use ramaria_service::Engine;

use super::retrieval::{SourceResolver, parse_label};

/// 基于工作库的单元证据解析器。
///
/// 字段约定:
/// - `cache`: `(层名, 主键)` → 覆盖的消息 id；缺失键表示不可映射或解析失败。
#[derive(Debug, Default)]
pub struct EngineSourceResolver {
    cache: HashMap<(String, String), Vec<String>>,
}

impl EngineSourceResolver {
    /// 预取一题集涉及的全部单元证据。
    ///
    /// 参数:
    /// - `engine`: 该条件使用的工作库引擎；
    /// - `labels`: 逐题轨迹里出现过的全部标签（可重复、可含不可映射前缀）。
    ///
    /// 返回:
    /// - 填充完毕的解析器；单个单元解析失败只记调试日志，按不可映射处理。
    pub async fn prefetch(engine: &Engine, labels: &[String]) -> Self {
        let mut cache: HashMap<(String, String), Vec<String>> = HashMap::new();
        for label in labels {
            let Some((layer, id)) = parse_label(label) else {
                continue;
            };
            let key = (layer.to_string(), id.to_string());
            if cache.contains_key(&key) {
                continue;
            }
            match engine.memory_unit_sources(layer, id).await {
                Ok(messages) => {
                    cache.insert(key, messages);
                }
                Err(error) => {
                    tracing::debug!(layer, error = %error, "单元证据解析失败，按不可映射处理");
                }
            }
        }
        Self { cache }
    }
}

impl SourceResolver for EngineSourceResolver {
    fn resolve(&mut self, layer: &str, id: &str) -> Option<Vec<String>> {
        self.cache
            .get(&(layer.to_string(), id.to_string()))
            .cloned()
    }
}
