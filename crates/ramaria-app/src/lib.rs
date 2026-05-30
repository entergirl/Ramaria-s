//! 应用编排层：用例调度，CLI 和 Desktop 共用。
//! 不直接处理 UI 展示。

pub use ramaria_core;

pub fn hello_app() -> &'static str {
    "ramaria-app is ready"
}
