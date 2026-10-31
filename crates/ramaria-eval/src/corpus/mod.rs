//! crates/ramaria-eval/src/corpus/mod.rs - Ramaria 评测语料模块
//!
//! 设计特点:
//! - 语料不随包、不入库、不联网：只校验用户自备的本地副本
//! - 注册表是唯一事实源：名称、版本、许可、来源、文件大小与哈希
//! - 校验与适配器分文件：注册表不读内容，适配器不碰文件哈希
//! - 中英对齐以 question_id 为键，人名与正文不参与
//! - 错误统一走 core 结构化错误，文案带路径、来源与期望值

mod lifebench;
mod registry;
mod verify;

pub use lifebench::{
    AlignmentReport, Message, Qa, Sample, ScorePoint, Session, align, load, load_language,
};
pub use registry::{CorpusFileSpec, CorpusLanguage, CorpusSpec, find, lifebench};
pub use verify::{FileCheck, VerifyReport, default_root, expected_path, verify, verify_command};

#[cfg(test)]
mod tests;
