//! crates/ramaria-memory/src/recall.rs - 召回装配共用实现（在线管线与服务层同源）
//!
//! 设计特点:
//! - 单份实现：把「检索 → Persona-Aware 过滤 → 衰减重排 → 预算裁剪 → 段落渲染 → utt 渲染」
//!   抽为共用函数，在线管线（Stage 5）与服务层 recall 用例调用同一份，保证两个入口召回同源
//! - 与宿主解耦：检索器与关键词镜像经只读视图 trait（`RetrieverSource` /
//!   `KeywordMirrorSource`）注入，本模块不依赖 app / cli / desktop / 协议概念
//! - 锁纪律：std 读锁只覆盖同步内存检索；embedding 生成与 touch_l1 在锁外 await
//! - 静默降级：嵌入不可用 → 向量通道缺席；镜像为空 → 关键词通道缺席；
//!   索引未加载 → 空召回（不视为故障，也不阻塞调用方）
//! - 隐私：不记录查询文本与记忆原文，日志只记计数、维度与字符数
mod assemble;
mod types;

pub use assemble::{assemble_recall, current_now_ms};
pub use types::{
    KeywordMirrorSource, RecallChannels, RecallGates, RecallHit, RecallInput, RecallMemoryLayers,
    RecallOutput, RetrieverSource,
};

#[cfg(test)]
mod tests;
