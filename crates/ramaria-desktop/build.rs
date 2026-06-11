//! rust/crates/ramaria-desktop/build.rs - Tauri 构建脚本 + 字体自动下载
//!
//! 设计特点:
//! - 调用 tauri_build::build() 生成 Tauri 运行时上下文代码
//! - 在 tauri-build 完成后，自动从 jsDelivr CDN（中国大陆有节点）
//!   下载 8 个字体文件（TTF 格式）到 frontend/fonts/ 目录
//! - 已存在的文件自动跳过（缓存），避免每次构建重复下载
//! - 下载失败不阻断构建 —— 仅输出 warning，系统字体回退生效
//! - jsDelivr 主 URL + GitHub raw 备用 URL，任一成功即可
//! - 使用 ureq（最小依赖阻塞 HTTP 客户端）+ 30s 超时
//!
//! 分发建议:
//! - 字体文件（~400KB）建议直接提交到 Git 仓库，彻底消除网络依赖
//! - 提交后 build.rs 自动跳过下载，CI 和离线构建均不受影响

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    // ── Tauri 上下文生成 ──
    tauri_build::build();

    // ── 字体自动下载 ──
    if let Err(e) = download_fonts() {
        println!(
            "cargo:warning=[ramaria-desktop] 字体自动下载失败，将使用系统回退字体。错误: {}",
            e
        );
        // 不 panic —— 下载失败是良性降级
    }
}

// =========================================================
// 字体下载逻辑
// =========================================================

/// 每个字体文件有两个候选 URL：jsDelivr CDN（主）和 GitHub raw（备用）。
///
/// jsDelivr 在中国大陆有 CDN 节点，访问速度远优于 GitHub raw；
/// GitHub raw 作为备用，确保海外用户也可正常下载。
///
/// 字段: (本地文件名, 主 URL, 备用 URL)
const FONTS: &[(&str, &str, &str)] = &[
    // ── DM Sans ──
    (
        "dm-sans-300.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmsans/DMSans-Light.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans-Light.ttf",
    ),
    (
        "dm-sans-400.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmsans/DMSans-Regular.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans-Regular.ttf",
    ),
    (
        "dm-sans-500.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmsans/DMSans-Medium.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans-Medium.ttf",
    ),
    (
        "dm-sans-600.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmsans/DMSans-SemiBold.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans-SemiBold.ttf",
    ),
    // ── DM Serif Display ──
    (
        "dm-serif-display.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmserifdisplay/DMSerifDisplay-Regular.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmserifdisplay/DMSerifDisplay-Regular.ttf",
    ),
    (
        "dm-serif-display-italic.ttf",
        "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/dmserifdisplay/DMSerifDisplay-Italic.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/dmserifdisplay/DMSerifDisplay-Italic.ttf",
    ),
    // ── JetBrains Mono ──
    (
        "jetbrains-mono-400.ttf",
        "https://cdn.jsdelivr.net/gh/JetBrains/JetBrainsMono@master/fonts/ttf/JetBrainsMono-Regular.ttf",
        "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/fonts/ttf/JetBrainsMono-Regular.ttf",
    ),
    (
        "jetbrains-mono-500.ttf",
        "https://cdn.jsdelivr.net/gh/JetBrains/JetBrainsMono@master/fonts/ttf/JetBrainsMono-Medium.ttf",
        "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/fonts/ttf/JetBrainsMono-Medium.ttf",
    ),
];

/// 下载超时时间（单文件单次尝试）。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// 自动下载缺失的字体文件。
///
/// 逻辑:
/// 1. 确定字体目录（`CARGO_MANIFEST_DIR/frontend/fonts/`）
/// 2. 确保目录存在
/// 3. 遍历 FONTS 列表，跳过已存在的文件
/// 4. 对缺失的文件，先尝试 jsDelivr CDN，失败则回退 GitHub raw
/// 5. 将下载内容写入目标路径
///
/// 返回:
/// - `Ok(())`: 全部文件已就绪
/// - `Err(String)`: 至少一个文件下载失败（已有文件不受影响）
fn download_fonts() -> Result<(), String> {
    // ── 确定字体目录 ──
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|e| format!("无法读取 CARGO_MANIFEST_DIR: {e}"))?;

    let fonts_dir = PathBuf::from(&manifest_dir).join("frontend").join("fonts");

    // 确保目录存在
    std::fs::create_dir_all(&fonts_dir)
        .map_err(|e| format!("无法创建字体目录 {}: {e}", fonts_dir.display()))?;

    println!(
        "cargo:warning=[ramaria-desktop] 字体目录: {}",
        fonts_dir.display()
    );

    // ── 逐文件处理 ──
    let mut skip_count = 0u32;
    let mut download_count = 0u32;
    let mut errors: Vec<String> = Vec::new();

    for (filename, primary_url, fallback_url) in FONTS {
        let dest = fonts_dir.join(filename);

        // 文件已存在 → 跳过
        if dest.exists() {
            skip_count += 1;
            continue;
        }

        // 文件不存在 → 下载（主 URL → 备用 URL）
        println!("cargo:warning=[ramaria-desktop] 下载字体: {filename}");

        // 先尝试 jsDelivr CDN
        match download_file(primary_url, &dest, "jsDelivr") {
            Ok(size) => {
                println!("cargo:warning=  -> jsDelivr 完成 ({size} bytes)");
                download_count += 1;
                continue;
            }
            Err(e) => {
                println!("cargo:warning=  -> jsDelivr 失败: {e}");
            }
        }

        // 回退 GitHub raw
        match download_file(fallback_url, &dest, "GitHub") {
            Ok(size) => {
                println!("cargo:warning=  -> GitHub 完成 ({size} bytes)");
                download_count += 1;
                continue;
            }
            Err(e) => {
                let msg = format!("{filename}: jsDelivr 和 GitHub 均失败 (最后错误: {e})");
                println!("cargo:warning=  -> GitHub 也失败");
                errors.push(msg);
            }
        }
    }

    // ── 汇总 ──
    println!(
        "cargo:warning=[ramaria-desktop] 字体就绪: {} 个已有, {} 个本次下载, {} 个失败",
        skip_count,
        download_count,
        errors.len()
    );

    if !errors.is_empty() {
        return Err(format!(
            "{} 个字体下载失败 ({} 个已存在，系统回退字体将生效): {}",
            errors.len(),
            skip_count,
            errors.join("; ")
        ));
    }

    Ok(())
}

/// 下载单个文件并保存到磁盘。
///
/// 参数:
/// - `url`: 远程文件 URL
/// - `dest`: 本地保存路径
/// - `source`: 来源标签（仅用于日志）
///
/// 返回:
/// - `Ok(u64)`: 下载的字节数
/// - `Err(String)`: 错误描述
fn download_file(url: &str, dest: &PathBuf, source: &str) -> Result<u64, String> {
    // ── 发起 HTTP GET ──
    let response = ureq::get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .call()
        .map_err(|e| {
            // ureq 错误类型多样，统一转为可读字符串
            match &e {
                ureq::Error::Transport(t) => {
                    format!("[{source}] 网络错误: {t}")
                }
                ureq::Error::Status(code, _resp) => {
                    format!("[{source}] HTTP {code}")
                }
            }
        })?;

    // 双重检查状态码（ureq v2 中 2xx 不会进 Error::Status）
    let status = response.status();
    if status != 200 {
        return Err(format!("[{source}] HTTP {status}"));
    }

    // ── 读取响应体 ──
    let mut body: Vec<u8> = Vec::new();
    response
        .into_reader()
        .take(5 * 1024 * 1024) // 单文件硬上限 5MB（字体文件通常 30-80KB）
        .read_to_end(&mut body)
        .map_err(|e| format!("[{source}] 读取失败: {e}"))?;

    let size = body.len() as u64;

    // 基本校验：字体文件不应太小
    if size < 1024 {
        return Err(format!(
            "[{source}] 内容过小 ({size} bytes)，非有效字体文件"
        ));
    }

    // ── 写入磁盘 ──
    std::fs::write(dest, &body)
        .map_err(|e| format!("[{source}] 写入 {} 失败: {e}", dest.display()))?;

    Ok(size)
}
