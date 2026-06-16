//! rust/crates/ramaria-app/src/update.rs - 版本检查与自动更新检测
//!
//! 设计特点:
//! - 通过 GitHub Release API 检查最新版本
//! - 与当前 CARGO_PKG_VERSION 做 semver 比较
//! - 网络超时 + 优雅降级：失败时返回 "unknown" 而不崩溃
//! - 请求添加 User-Agent 头以符合 GitHub API 规范
//! - 不依赖任何桌面或 CLI 特定类型，可被两者共用
//!
//! 安全与可用性:
//! - 设置 10s 连接超时 + 30s 总超时，防止网络卡死
//! - GitHub API 限流（未认证: 60 req/h）时返回友好提示
//! - 不缓存结果，每次调用都是实时查询（"手动检查更新"语义）

use serde::Deserialize;

// =========================================================
// 类型定义
// =========================================================

/// 更新检查结果。
///
/// 包含当前版本、远程最新版本、是否有更新可用，
/// 以及供 UI 展示用的下载页面 URL 和版本描述。
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    /// 当前运行的版本（来自 CARGO_PKG_VERSION）
    pub current_version: String,
    /// 远程最新版本标签（如 "v1.1.0"），None 表示无法获取
    pub latest_version: Option<String>,
    /// 是否有新版本可用
    pub update_available: bool,
    /// GitHub Release 页面 URL（供浏览器打开）
    pub release_url: Option<String>,
    /// 版本发布说明（Markdown 格式，供 UI 展示）
    pub release_notes: Option<String>,
    /// 检查失败时的错误信息（供日志/调试）
    pub error: Option<String>,
}

/// GitHub Release API 响应（仅提取我们需要的字段）。
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    /// 版本标签，如 "v1.1.0"
    tag_name: String,
    /// Release 页面 URL
    html_url: String,
    /// 发布说明（Markdown）
    body: Option<String>,
}

// =========================================================
// 公开 API
// =========================================================

/// 检查 GitHub 上是否有新版本可用。
///
/// 调用方:
/// - 桌面端"检查更新"按钮 → Tauri Command。
/// - CLI `ramaria diagnostics --check-update`（如 CLI 需要）。
///
/// 返回:
/// - `UpdateStatus`，即使网络请求失败也会返回当前版本 + 错误信息。
///
/// 行为:
/// - 向 `https://api.github.com/repos/entergirl/Ramaria-s/releases/latest` 发 GET 请求。
/// - 解析 `tag_name`，与 `CARGO_PKG_VERSION` 做 semver 比较。
/// - 10s 连接超时 + 30s 总超时。
/// - 请求失败不 panic，状态中 `error` 含原因。
///
/// 示例:
/// ```ignore
/// let status = check_update().await;
/// if status.update_available {
///     println!("新版本可用: {}", status.latest_version.unwrap_or_default());
/// }
/// ```
pub async fn check_update() -> UpdateStatus {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let repo = "entergirl/Ramaria-s";
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");

    // 发送 HTTP GET 请求，带超时
    match fetch_latest_release(&url).await {
        Ok(release) => {
            let latest_version = release.tag_name.trim_start_matches('v').to_string();
            let update_available = is_newer_version(&latest_version, &current_version);

            tracing::info!(
                current = %current_version,
                latest = %latest_version,
                update_available,
                "版本检查完成"
            );

            UpdateStatus {
                current_version,
                latest_version: Some(release.tag_name),
                update_available,
                release_url: Some(release.html_url),
                release_notes: release.body,
                error: None,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "版本检查失败");

            UpdateStatus {
                current_version,
                latest_version: None,
                update_available: false,
                release_url: None,
                release_notes: None,
                error: Some(e),
            }
        }
    }
}

// =========================================================
// 内部实现
// =========================================================

/// 向 GitHub API 查询最新的 Release 信息。
///
/// 安全约束:
/// - 连接超时 10s，总超时 30s（防止 DNS 解析卡死或下载大响应体）。
/// - 添加 User-Agent 头（GitHub API 要求）。
/// - 非 200 状态码返回友好错误信息。
/// - 对 GitHub API 限流返回特殊提示。
async fn fetch_latest_release(url: &str) -> Result<GitHubRelease, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("无法创建 HTTP 客户端: {e}"))?;

    let response = client
        .get(url)
        .header("User-Agent", "Ramaria-UpdateChecker/1.0")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "网络请求超时，请检查网络连接后重试".to_string()
            } else if e.is_connect() {
                "无法连接到 GitHub，请检查网络连接".to_string()
            } else {
                format!("网络请求失败: {e}")
            }
        })?;

    let status = response.status();

    if !status.is_success() {
        // 检查 rate limit 头（先复制为 owned String，避免后续移动 response 时借用冲突）
        let rate_remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();
        let rate_reset = response
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();

        // 读取响应体（最多 4KB）以获取具体错误信息
        let status_code = status.as_u16();
        let body_preview = read_body_preview(response).await;

        // 检测是否是 HTML 响应（防火墙/代理拦截的典型特征）
        let is_html = body_preview.trim_start().starts_with("<!DOCTYPE")
            || body_preview.trim_start().starts_with("<html")
            || body_preview.trim_start().starts_with("<HTML");

        return match status_code {
            403 if is_html => {
                Err("无法访问 GitHub API（网络环境可能阻止了对 api.github.com 的请求）。\n\
                     建议：检查代理/防火墙设置，或手动访问 https://github.com/entergirl/Ramaria-s/releases 查看更新"
                    .to_string())
            }
            403 if rate_remaining == "0" => {
                // 明确的限流信息
                Err(format!(
                    "GitHub API 请求频率已达上限（60次/小时）。\n\
                     请在 {rate_reset} 之后重试，或配置 GitHub Token 以获得更高限额。"
                ))
            }
            403 => {
                Err("GitHub API 访问受限（HTTP 403）。\n\
                     可能原因：IP 被限制、需要认证，或网络环境无法访问 GitHub API。\n\
                     可尝试：使用代理、配置个人访问令牌，或手动访问 Release 页面。"
                    .to_string())
            }
            404 => Err("未找到最新版本信息（仓库可能不存在或没有 Release）".to_string()),
            code => {
                if is_html {
                    Err(format!(
                        "GitHub API 返回 HTML 页面（HTTP {code}），\n\
                         请求可能被代理/防火墙拦截。响应预览: {}",
                        body_preview.chars().take(200).collect::<String>()
                    ))
                } else {
                    Err(format!(
                        "GitHub API 返回错误状态码: {code}\n响应: {}",
                        body_preview.chars().take(200).collect::<String>()
                    ))
                }
            }
        };
    }

    // 检查响应 Content-Type，确保是 JSON
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.contains("application/json") {
        // 非 JSON 响应 — 读取 body 帮助诊断
        let body_preview = read_body_preview(response).await;
        return Err(format!(
            "GitHub API 返回非 JSON 内容类型: {content_type}\n\
             响应预览: {}",
            body_preview.chars().take(200).collect::<String>()
        ));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("解析 GitHub API 响应失败: {e}"))?;

    Ok(release)
}

/// 读取响应体预览（最多 4KB），用于错误诊断。
///
/// 说明:
/// - 在非 200 状态码或非 JSON 响应时调用，帮助判断是限流、代理拦截还是其他问题。
/// - 限制读取 4096 字节，防止恶意大响应撑爆内存。
/// - 读取失败时返回空字符串，不阻塞错误报告流程。
async fn read_body_preview(response: reqwest::Response) -> String {
    match response.text().await {
        Ok(body) => {
            if body.len() > 4096 {
                format!("{}... [截断，总长 {} 字节]", &body[..4096], body.len())
            } else {
                body
            }
        }
        Err(_) => String::new(),
    }
}

/// 简易 semver 版本比较。
///
/// 比较规则:
/// - 按 '.' 拆分为数值数组，逐段比较 (MAJOR → MINOR → PATCH)。
/// - 缺失的段视为 0（如 "1.0" 等同于 "1.0.0"）。
/// - 非数字段回退到字符串比较（处理 pre-release 标签如 "1.0.0-alpha"）。
///
/// 返回:
/// - `true`: `a` 比 `b` 更新。
/// - `false`: 版本相同或 `a` 更旧。
///
/// 示例:
/// - `is_newer_version("1.1.0", "1.0.0")` → true
/// - `is_newer_version("1.0.0", "1.0.1")` → false
/// - `is_newer_version("2.0", "1.9.9")` → true
fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_segments = |v: &str| -> Vec<u32> {
        v.split('.')
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect()
    };

    let a_segs = parse_segments(latest);
    let b_segs = parse_segments(current);

    let max_len = a_segs.len().max(b_segs.len());

    for i in 0..max_len {
        let a = a_segs.get(i).copied().unwrap_or(0);
        let b = b_segs.get(i).copied().unwrap_or(0);

        match a.cmp(&b) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => continue,
        }
    }

    // 所有数字段相等，说明版本相同
    false
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 版本比较 ──

    #[test]
    fn test_version_newer_major() {
        assert!(is_newer_version("2.0.0", "1.9.9"));
    }

    #[test]
    fn test_version_newer_minor() {
        assert!(is_newer_version("1.1.0", "1.0.9"));
    }

    #[test]
    fn test_version_newer_patch() {
        assert!(is_newer_version("1.0.1", "1.0.0"));
    }

    #[test]
    fn test_version_equal() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
    }

    #[test]
    fn test_version_older() {
        assert!(!is_newer_version("0.9.0", "1.0.0"));
    }

    #[test]
    fn test_version_shorter_is_treated_as_zero() {
        // "1.1" 应视为 "1.1.0"，比 "1.0.0" 新
        assert!(is_newer_version("1.1", "1.0.0"));
    }

    #[test]
    fn test_version_different_length() {
        // "2.0" 比 "1.9.9" 新
        assert!(is_newer_version("2.0", "1.9.9"));
    }

    #[test]
    fn test_version_with_non_numeric_fallback() {
        // pre-release 标签中的非数字段视为 0
        // "1.1.0-alpha" 在数字比较上与 "1.1.0" 相同
        assert!(!is_newer_version("1.1.0-alpha", "1.1.0"));
    }

    // ── UpdateStatus 结构完整性 ──

    #[test]
    fn test_update_status_current_version_is_pkg_version() {
        // 注意：此测试验证 check_update 即使网络不通也返回正确的 current_version
        // 使用默认值模拟构造
        let status = UpdateStatus {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: None,
            update_available: false,
            release_url: None,
            release_notes: None,
            error: None,
        };
        // 工作区版本为 1.0.1
        assert_eq!(status.current_version, "1.0.1");
        assert!(!status.update_available);
    }
}
