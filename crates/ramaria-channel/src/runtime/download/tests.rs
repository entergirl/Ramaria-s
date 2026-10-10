//! crates/ramaria-channel/src/runtime/download/tests.rs - 图片下载模块单元测试
//!
//! 设计特点:
//! - 本机最小 HTTP 服务端驱动成功 / 404 / 超限三类分支，不依赖外网
//! - 构造参数与默认上限断言；空地址显式报错

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 启动一次性最小 HTTP 服务端（读一次请求后回固定响应）。
async fn spawn_http_server(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定应成功");
    let addr = listener.local_addr().expect("地址应可读");
    let _server = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer).await;
            let _ = stream.write_all(&response).await;
            let _ = stream.shutdown().await;
        }
    });
    format!("http://{addr}/x")
}

/// 空地址：显式不发起请求。
#[tokio::test]
async fn empty_url_is_rejected() {
    let downloader = HttpImageDownloader::new();
    let error = downloader.download("   ").await.expect_err("空地址应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
}

/// 构造参数：默认上限对齐理解链路；0 回落默认值。
#[test]
fn max_bytes_defaults_and_override() {
    assert_eq!(HttpImageDownloader::new().max_bytes(), MAX_IMAGE_BYTES);
    assert_eq!(HttpImageDownloader::default().max_bytes(), MAX_IMAGE_BYTES);
    assert_eq!(
        HttpImageDownloader::with_max_bytes(0).max_bytes(),
        MAX_IMAGE_BYTES
    );
    assert_eq!(HttpImageDownloader::with_max_bytes(1024).max_bytes(), 1024);
}

/// 下载成功：返回完整字节。
#[tokio::test]
async fn download_success_returns_bytes() {
    let body = b"hello-image";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut raw = response.into_bytes();
    raw.extend_from_slice(body);
    let url = spawn_http_server(raw).await;

    let downloader = HttpImageDownloader::new();
    let bytes = downloader.download(&url).await.expect("下载应成功");
    assert_eq!(bytes, body);
}

/// 非 2xx：协议错误（不含 URL 原文）。
#[tokio::test]
async fn download_non_success_is_protocol_error() {
    let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let url = spawn_http_server(raw).await;

    let downloader = HttpImageDownloader::new();
    let error = downloader.download(&url).await.expect_err("404 应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(
        !error.to_string().contains(&url),
        "错误文案不应包含完整地址"
    );
    assert!(error.to_string().contains("404"));
}

/// 响应头预检：Content-Length 超过上限直接失败。
#[tokio::test]
async fn download_over_limit_is_rejected_by_header() {
    let response =
        "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n".to_string();
    let url = spawn_http_server(response.into_bytes()).await;

    let downloader = HttpImageDownloader::with_max_bytes(16);
    let error = downloader.download(&url).await.expect_err("超限应失败");
    assert!(matches!(error, ChannelError::Protocol(_)));
    assert!(
        !error.to_string().contains(&url),
        "错误文案不应包含完整地址"
    );
}
