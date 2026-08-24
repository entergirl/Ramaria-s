//! vendor/candle-kernels/build.rs - Ramaria 本地补丁的 candle-kernels 构建脚本
//!
//! 设计特点:
//! - 编译 CUDA 内核源文件（src/*.cu）为 PTX 机器码与静态库，供 candle-core CUDA 后端调用
//! - PTX 构建：扫描 src/ 下全部 .cu（排除 moe_*），产出 ptx.rs 供 lib.rs 内联嵌入
//! - moe 构建：单独编译 MoE 相关 .cu 为 libmoe.a 静态库并链接 cudart
//! - 来源为 crates.io 的 candle-kernels，本地仅修改 MSVC 编译参数（见下），
//!   用于修复 Windows + CUDA 13.x 下 quantized.cu 因 CCCL 要求标准符合预处理器而失败
//! - 仅在启用 cuda feature 且目标平台为 MSVC 时改变行为，其余平台保持上游原逻辑
//! - 非 CUDA（默认）构建不引入本 crate，无任何行为差异

use cudaforge::{KernelBuilder, Result};
use std::env;
use std::path::PathBuf;

fn main() -> Result<()> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/compatibility.cuh");
    println!("cargo::rerun-if-changed=src/cuda_utils.cuh");
    println!("cargo::rerun-if-changed=src/binary_op_macros.cuh");

    // 检测 MSVC 目标平台（Windows + cl.exe）。
    // 需在 PTX 与 moe 两个 KernelBuilder 之前确定，用于注入 MSVC 专属编译参数。
    let mut is_target_msvc = false;
    if let Ok(target) = std::env::var("TARGET") {
        if target.contains("msvc") {
            is_target_msvc = true;
        }
    }

    // Build for PTX
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ptx_path = out_dir.join("ptx.rs");
    let mut ptx_builder = KernelBuilder::new()
        .source_dir("src") // Scan src/ for .cu files
        .exclude(&["moe_*.cu"]) // Exclude moe kernels for ptx build
        .arg("--expt-relaxed-constexpr")
        .arg("-std=c++17")
        .arg("-O3");

    // MSVC 修复：CUDA 13.x 的 CCCL 头文件要求标准符合预处理器（/Zc:preprocessor）。
    // nvcc 在 Windows 上调用 cl.exe 时不转发该参数，quantized.cu（内含 CCCL 头）
    // 触发 fatal error C1189 导致整个 PTX 构建失败。此处显式通过 -Xcompiler
    // 把 /Zc:preprocessor 转交给 cl.exe，使 quantized.cu 正常编译。
    // 参考：huggingface/candle#3686。
    if is_target_msvc {
        ptx_builder = ptx_builder.arg("-Xcompiler=/Zc:preprocessor");
    }

    let bindings = ptx_builder.build_ptx()?;

    bindings.write(&ptx_path)?;

    let mut moe_builder = KernelBuilder::default()
        .source_files(vec![
            "src/moe/moe_gguf.cu",
            "src/moe/moe_wmma.cu",
            "src/moe/moe_wmma_gguf.cu",
        ])
        .arg("--expt-relaxed-constexpr")
        .arg("-std=c++17")
        .arg("-O3");

    // Disable bf16 WMMA kernels on GPUs older than sm_80 (Ampere).
    // bf16 WMMA fragments require compute capability >= 8.0.
    let compute_cap = cudaforge::detect_compute_cap()
        .map(|arch| arch.base())
        .unwrap_or(80);
    if compute_cap < 80 {
        moe_builder = moe_builder.arg("-DNO_BF16_KERNEL");
    }

    if is_target_msvc {
        moe_builder = moe_builder.arg("-D_USE_MATH_DEFINES");
        // 同 PTX 构建：moe_gguf.cu 含 CCCL 头，需标准符合预处理器。
        moe_builder = moe_builder.arg("-Xcompiler=/Zc:preprocessor");
    }

    if !is_target_msvc {
        moe_builder = moe_builder.arg("-Xcompiler").arg("-fPIC");
    }

    moe_builder.build_lib(out_dir.join("libmoe.a"))?;
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-lib=moe");
    println!("cargo:rustc-link-lib=dylib=cudart");
    if !is_target_msvc {
        println!("cargo:rustc-link-lib=stdc++");
    }
    Ok(())
}
