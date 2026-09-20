//! crates/ramaria-app/src/persona_prompt.rs - persona.toml 冷启动 System Prompt 加载模块
//!
//! 设计特点:
//! - persona.toml 冷启动 system prompt 的薄委托（实现见
//!   `ramaria_memory::chat::load_persona_toml_prompt`，与 service / MCP 入口同源）
//! - 保留 app 侧唯一入口（未接线 Stage 与既有调用方签名不变）
//! - 解析失败 / 文件缺失返回 `None`，由上层降级到默认 Ramaria prompt

/// 尝试加载 persona.toml 并构建有温度的基础 system prompt。
///
/// 数据来源优先级:
/// 1. `db_config`: 从 DB persona.config 中读取的 TOML 内容（setup 时写入）
/// 2. 文件系统回退: `../config/personas/rama-0001.toml`，其次旧路径
///    `../config/persona.toml`（未迁移的旧安装兼容回退）
///
/// 参数:
/// - `db_config`: DB persona.config 内容（None 时直接走文件系统回退）。
///
/// 返回:
/// - `Some(prompt)`: 由 `A_persona` + `E_rules`（显式优先，缺省共享规则）组装的基础 prompt。
/// - `None`: 解析失败 / 文件缺失 —— 由上层降级到默认 Ramaria prompt。
pub(crate) fn load_persona_toml_prompt(db_config: Option<&str>) -> Option<String> {
    ramaria_memory::chat::load_persona_toml_prompt(db_config)
}
