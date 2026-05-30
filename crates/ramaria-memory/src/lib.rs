//! 记忆系统：L1/L2/L3、衰减、图谱、冲突检测。
//! 只依赖 LLM trait，不依赖具体 provider。

pub fn hello_memory() -> &'static str {
    "ramaria-memory is ready"
}
