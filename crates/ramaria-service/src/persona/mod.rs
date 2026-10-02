//! crates/ramaria-service/src/persona/mod.rs - Ramaria 人格读取、管理与重生成用例
//!
//! 设计特点:
//! - 人格读取：人格摘要列表与人格卡片（性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度）
//! - 人格管理：全字段列表、基本信息更新、人格文件导入（目录扫描文件名 = uid / 单文件显式 uid；单文件失败不中断）
//! - 人格重生成：导入失败后的离线重建路径（全量消息枚举 → 会话去重 → 逐会话 L1 重生成，含连续失败早停）
//! - 严格按 persona_uid 隔离：读取与重生成只处理目标人格的记录，不跨人格聚合
//! - 逐段独立降级：卡片任一段读取失败记 warn 并返回空段，不阻塞整张卡片；文件导入单文件失败转为结果条目
//! - 条目上限：卡片各段按 `view::MAX_CARD_ITEMS` 截断（避免大库把整张卡片撑爆）
//! - 隐私：卡片不含 utt 原文块；日志中的个人标识经 `mask_id` 脱敏
//!
//! 模块划分:
//! - `load`：人格文件导入（目录扫描 / 单文件显式 uid；单文件失败转为结果条目）；
//! - `regenerate`：人格 L1 重生成（离线重建路径，含连续失败早停）；
//! - `update`：人格基本信息更新与系统用户人格保障；
//! - `view`：人格读取与视图组装（摘要列表 / 全字段列表 / 卡片 / 成熟度计数）。

mod load;
mod regenerate;
mod update;
mod view;

// 类型 re-export：`crate::persona::{PersonaLoadMode, PersonaRegenerateOutcome}` 为入口层既有调用路径
pub use load::PersonaLoadMode;
pub use regenerate::PersonaRegenerateOutcome;

// 用例 re-export：`crate::persona::X` 为引擎既有调用路径
pub(crate) use load::{load_file, load_from_dir};
pub(crate) use regenerate::regenerate_import_l1;
pub(crate) use update::{ensure_user, update_info};
pub(crate) use view::{card, list, list_full};

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
