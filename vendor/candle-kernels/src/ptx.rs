//! vendor/candle-kernels/src/ptx.rs - Ramaria 本地补丁的 candle-kernels PTX 内联常量
//!
//! 设计特点:
//! - 以编译期常量承载 build.rs 生成的各内核 PTX 机器码
//! - 经 include_str! 把 .ptx 文本内联进二进制，运行时免外部文件
//! - 内核清单与 lib.rs 的 Id 枚举一一对应
//! - 来源为 crates.io 的 candle-kernels，本地未改动，仅加项目规范注释

pub const AFFINE: &str = include_str!(concat!(env!("OUT_DIR"), "/affine.ptx"));
pub const BINARY: &str = include_str!(concat!(env!("OUT_DIR"), "/binary.ptx"));
pub const CAST: &str = include_str!(concat!(env!("OUT_DIR"), "/cast.ptx"));
pub const CONV: &str = include_str!(concat!(env!("OUT_DIR"), "/conv.ptx"));
pub const FILL: &str = include_str!(concat!(env!("OUT_DIR"), "/fill.ptx"));
pub const INDEXING: &str = include_str!(concat!(env!("OUT_DIR"), "/indexing.ptx"));
pub const QUANTIZED: &str = include_str!(concat!(env!("OUT_DIR"), "/quantized.ptx"));
pub const REDUCE: &str = include_str!(concat!(env!("OUT_DIR"), "/reduce.ptx"));
pub const SORT: &str = include_str!(concat!(env!("OUT_DIR"), "/sort.ptx"));
pub const TERNARY: &str = include_str!(concat!(env!("OUT_DIR"), "/ternary.ptx"));
pub const UNARY: &str = include_str!(concat!(env!("OUT_DIR"), "/unary.ptx"));
