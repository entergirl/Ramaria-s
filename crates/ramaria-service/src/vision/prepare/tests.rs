//! crates/ramaria-service/src/vision/prepare/tests.rs - Ramaria 图片输入准备用例
//!
//! 设计特点:
//! - 图片字节在内存中构造（真实编码器产出），不访问文件系统、不连网
//! - 覆盖格式识别、头部尺寸解析与透传 / 缩放 / 转换 / 拒绝四类准备结论

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use std::io::Cursor;

use super::*;

// ===== 脚手架 =====

/// 生成指定尺寸的真实 JPEG 字节。
fn jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
    let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(width, height, |x, _| Rgb([(x % 256) as u8, 120, 60]));
    let mut buffer = Vec::new();
    DynamicImage::ImageRgb8(image)
        .write_to(&mut Cursor::new(&mut buffer), ImageFormat::Jpeg)
        .expect("编码 JPEG 应成功");
    buffer
}

/// 生成指定尺寸的真实 BMP 字节。
fn bmp_bytes(width: u32, height: u32) -> Vec<u8> {
    let image: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgb([10, 200, 30]));
    let mut buffer = Vec::new();
    DynamicImage::ImageRgb8(image)
        .write_to(&mut Cursor::new(&mut buffer), ImageFormat::Bmp)
        .expect("编码 BMP 应成功");
    buffer
}

/// 解码图片读取尺寸。
fn decoded_size(bytes: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory(bytes).expect("解码应成功");
    (image.width(), image.height())
}

/// 构造最小 PNG 头（签名 + IHDR；不含图像数据）。
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// 构造最小 GIF 头（签名 + 逻辑屏幕描述符）。
fn gif_header(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes
}

// ===== 格式识别 =====

/// 魔数识别覆盖五种格式，未知内容返回 None。
#[test]
fn detect_kind_matches_magic_numbers() {
    assert_eq!(
        detect_kind(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some(ImageKind::Jpeg)
    );
    assert_eq!(
        detect_kind(&[0x89, 0x50, 0x4E, 0x47, 0x0D]),
        Some(ImageKind::Png)
    );
    assert_eq!(detect_kind(b"GIF89a"), Some(ImageKind::Gif));
    assert_eq!(
        detect_kind(b"RIFF\x00\x00\x00\x00WEBPVP8X"),
        Some(ImageKind::Webp)
    );
    assert_eq!(detect_kind(b"BM\x36\x00"), Some(ImageKind::Bmp));
    assert_eq!(detect_kind(b"II*\x00"), None);
    assert_eq!(detect_kind(b""), None);
}

/// 头部尺寸解析：PNG / GIF 按各自偏移；JPEG 扫段取 SOF。
#[test]
fn header_dimensions_parses_supported_formats() {
    let png = png_header(120, 340);
    assert_eq!(png_dimensions(&png), Some((120, 340)));

    let gif = gif_header(64, 48);
    assert_eq!(gif_dimensions(&gif), Some((64, 48)));

    let jpeg = jpeg_bytes(37, 91);
    assert_eq!(jpeg_dimensions(&jpeg), Some((37, 91)));

    // 截断到签名后：无法解析
    assert_eq!(jpeg_dimensions(&jpeg[..3]), None);
    assert_eq!(png_dimensions(&png[..20]), None);
    assert_eq!(gif_dimensions(&gif[..8]), None);
}

// ===== 输入准备 =====

/// 本地 provider：越界图片原样透传，MIME 取调用方口径。
#[test]
fn local_provider_passes_bytes_through() {
    let bytes = jpeg_bytes(100, 9000);
    let prepared = prepare_image(false, &bytes, "image/png").expect("本地透传不应拒绝");
    assert_eq!(prepared.mime, "image/png");
    assert!(matches!(prepared.bytes, Cow::Borrowed(_)));
    assert_eq!(prepared.bytes.as_ref(), bytes.as_slice());
}

/// 线上 provider：未越界的受支持格式原字节透传（MIME 按内容判定）。
#[test]
fn online_within_limit_passes_through() {
    let bytes = jpeg_bytes(320, 240);
    let prepared = prepare_image(true, &bytes, "image/jpeg").expect("未越界不应拒绝");
    assert_eq!(prepared.mime, "image/jpeg");
    assert!(matches!(prepared.bytes, Cow::Borrowed(_)));
    assert_eq!(prepared.bytes.as_ref(), bytes.as_slice());
}

/// 线上 provider：越界长图等比缩放到上限内并重编码为 JPEG。
#[test]
fn online_oversized_image_is_scaled_down() {
    let bytes = jpeg_bytes(202, 9000);
    let prepared = prepare_image(true, &bytes, "image/jpeg").expect("长图应缩放而非拒绝");
    assert_eq!(prepared.mime, "image/jpeg");
    assert!(matches!(prepared.bytes, Cow::Owned(_)));
    let (width, height) = decoded_size(&prepared.bytes);
    assert_eq!(height, MAX_ONLINE_SIDE, "长边应压到上限");
    assert!(width < 202, "宽度应等比缩小");
    assert!(width.max(height) <= MAX_ONLINE_SIDE);
}

/// 线上 provider：不受支持但可解码的格式转换为 JPEG。
#[test]
fn online_unsupported_but_decodable_is_converted() {
    let bytes = bmp_bytes(120, 60);
    let prepared = prepare_image(true, &bytes, "image/bmp").expect("BMP 应转换为 JPEG");
    assert_eq!(prepared.mime, "image/jpeg");
    assert!(matches!(prepared.bytes, Cow::Owned(_)));
    assert_eq!(decoded_size(&prepared.bytes), (120, 60));
}

/// 线上 provider：无法识别的内容按格式不受支持拒绝。
#[test]
fn online_unknown_content_is_rejected() {
    let result = prepare_image(true, b"II*\x00not-an-image", "image/jpeg");
    assert_eq!(result.err(), Some(ImageReject::UnsupportedFormat));
}

/// 线上 provider：越界且解码被内存上限拦下 —— 按不可解码拒绝。
#[test]
fn online_oversized_undecodable_is_rejected() {
    // 声明尺寸 20000×20000：解码需求远超内存上限，受限解码直接失败
    let bytes = png_header(20_000, 20_000);
    assert_eq!(png_dimensions(&bytes), Some((20_000, 20_000)));
    let result = prepare_image(true, &bytes, "image/png");
    assert_eq!(result.err(), Some(ImageReject::Undecodable));
}

/// 线上 provider：受支持格式但头部不可解析且解码失败 —— 透传交由后端判定。
#[test]
fn online_unparsable_but_accepted_passes_through() {
    let mut bytes = jpeg_bytes(64, 64);
    bytes.truncate(3);
    let prepared = prepare_image(true, &bytes, "image/jpeg").expect("应透传而非拒绝");
    assert_eq!(prepared.mime, "image/jpeg");
    assert_eq!(prepared.bytes.as_ref(), bytes.as_slice());
}

/// data URI 形态：MIME 与 base64 载荷按标准字母表组装。
#[test]
fn prepared_image_builds_data_uri() {
    let bytes = jpeg_bytes(32, 32);
    let prepared = prepare_image(true, &bytes, "image/jpeg").expect("未越界不应拒绝");
    let uri = prepared.to_data_uri();
    assert!(uri.starts_with("data:image/jpeg;base64,"));
    let expected = format!(
        "data:image/jpeg;base64,{}",
        super::super::base64_encode(&bytes)
    );
    assert_eq!(uri, expected);
}

/// MIME 兜底：扩展名与魔数两路都不识别时回退 image/jpeg。
#[test]
fn mime_helpers_fall_back_to_jpeg() {
    assert_eq!(image_mime_for("a.unknown"), "image/jpeg");
    assert_eq!(image_mime_from_bytes(b"not-an-image"), "image/jpeg");
    assert_eq!(image_mime_from_bytes(b"BM\x36\x00"), "image/bmp");
}
