//! crates/ramaria-desktop/src/commands/import_cmd.rs - QQ 聊天记录导入 Tauri Commands 模块入口
//!
//! 设计特点:
//! - 按职责分部：解析报告 / 格式检测 / 导入执行 / 进度转发
//! - 子模块命令在此重导出，命令注册路径与前端契约保持不变
//! - 快速导入（fast）：L0 写入后生成 L1 摘要，不触发深度级联
//! - 深度导入（deep）：L0 → L1 摘要 → 触发 L2/L3 级联
//! - 双画像支持——分别为导出者和对方创建独立 persona（由服务层用例承担）
//! - 路径安全校验：三层防御（canonicalize + 白名单 + 符号链接拒绝），复用 path_guard 模块
//! - 进度事件字段与前端契约一致；阶段进度由服务层用例发出，桌面只做转发
//! - 隐私回归断言（tests.rs）静态扫描本模块的日志与 instrument 写法

mod analyze;
mod detect;
mod progress;
mod result;
mod run;

pub use analyze::analyze_qq_chat;
pub use detect::detect_qq_format;
pub(crate) use progress::TauriImportProgressSink;
pub use run::import_qq_chat;

// 命令宏与命令同路径导出：命令注册按「模块路径::命令名」解析命令与其宏
#[doc(hidden)]
pub use analyze::{__cmd__analyze_qq_chat, __tauri_command_name_analyze_qq_chat};
#[doc(hidden)]
pub use detect::{__cmd__detect_qq_format, __tauri_command_name_detect_qq_format};
#[doc(hidden)]
pub use run::{__cmd__import_qq_chat, __tauri_command_name_import_qq_chat};

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
