//! crates/ramaria-service/src/vision/prepare.rs - Ramaria 图片理解输入准备模块
//!
//! 设计特点:
//! - 线上多模态后端的输入约束集中在本模块单点：单边像素上限 + 按内容判定的格式范围；
//! - 未越界且格式受支持时原字节透传（不解码），仅越界或需转换格式时才解码重编码；
//! - 本地 provider 原样透传（不受线上约束，与既有图片理解口径一致）；
//! - 仅在"确定会被后端拒收"时拒绝（格式不受支持且无法转换 / 越界且解码失败）；
//!   自身解码器读不出的图片仍透传，交由后端判定；
//! - 解码受内存上限约束，异常输入按拒绝降级，不 panic。

use std::borrow::Cow;
use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader};

// =========================================================
// 常量
// =========================================================

/// 线上多模态后端的单边像素上限（超出由后端拒收）。
const MAX_ONLINE_SIDE: u32 = 8192;

/// 缩放重编码的 JPEG 质量。
const RESIZE_JPEG_QUALITY: u8 = 85;

/// 解码内存上限（字节；超限按不可解码处置，避免异常输入耗尽内存）。
const DECODE_MEMORY_LIMIT: u64 = 256 * 1024 * 1024;

// =========================================================
// 公开类型
// =========================================================

/// 图片输入准备结论。
pub(crate) struct PreparedImage<'a> {
    /// 声明的 MIME：本地透传为调用方口径，线上为按内容判定。
    pub(crate) mime: &'static str,
    /// 待送出字节：未重编码时为原字节借用。
    pub(crate) bytes: Cow<'a, [u8]>,
}

impl PreparedImage<'_> {
    /// 组装 data URI（`data:{mime};base64,{payload}`）。
    pub(crate) fn to_data_uri(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime,
            super::base64_encode(&self.bytes)
        )
    }
}

/// 输入拒绝原因（调用方按降级口径标记跳过，不发起调用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageReject {
    /// 内容格式不受线上后端支持，且无法解码转换。
    UnsupportedFormat,
    /// 单边超过线上后端上限，且解码 / 缩放失败。
    Undecodable,
}

impl ImageReject {
    /// 日志用原因标签。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ImageReject::UnsupportedFormat => "unsupported_format",
            ImageReject::Undecodable => "undecodable",
        }
    }
}

// =========================================================
// 输入准备
// =========================================================

/// 按目标后端约束准备图片输入。
///
/// 口径:
/// - 本地 provider（`online = false`）：原字节透传，MIME 取 `fallback_mime`；
/// - 线上 provider：格式受支持且单边未越界时透传；越界时解码等比缩放到上限内
///   并重编码为 JPEG；不受支持的格式尝试转换为 JPEG。
///
/// 参数:
/// - `online`: 目标是否为线上 provider；
/// - `bytes`: 图片字节；
/// - `fallback_mime`: 本地透传使用的 MIME（调用方口径）。
///
/// 返回:
/// - `Ok`: 待送出字节与 MIME；
/// - `Err`: 确定会被后端拒收的原因。
pub(crate) fn prepare_image<'a>(
    online: bool,
    bytes: &'a [u8],
    fallback_mime: &'static str,
) -> Result<PreparedImage<'a>, ImageReject> {
    if !online {
        return Ok(borrowed(fallback_mime, bytes));
    }

    let Some(kind) = detect_kind(bytes) else {
        return Err(ImageReject::UnsupportedFormat);
    };

    // 头部尺寸（仅受支持格式解析；其余格式一律走解码判定）
    let header_size = match kind.accepted_online() {
        true => header_dimensions(bytes, kind),
        false => None,
    };

    // ---- 快路径：受支持格式且头部尺寸未越界 —— 原字节透传（不解码） ----
    if let Some((width, height)) = header_size {
        if width.max(height) <= MAX_ONLINE_SIDE {
            return Ok(borrowed(kind.mime(), bytes));
        }
    }

    // ---- 慢路径：需缩放 / 需转换格式 / 尺寸不明 —— 解码判定 ----
    let Some(image) = decode(bytes) else {
        if header_size.is_some() {
            // 已确认越界且解码失败：后端必然拒收，直接降级
            return Err(ImageReject::Undecodable);
        }
        if kind.accepted_online() {
            // 格式受支持但尺寸不明：透传交由后端判定
            return Ok(borrowed(kind.mime(), bytes));
        }
        return Err(ImageReject::UnsupportedFormat);
    };
    if kind.accepted_online() && image.width().max(image.height()) <= MAX_ONLINE_SIDE {
        // 解码后才确认未越界（头部尺寸缺失场景）：原字节透传
        return Ok(borrowed(kind.mime(), bytes));
    }

    let scaled = fit_within(image, MAX_ONLINE_SIDE);
    let encoded = encode_jpeg(&scaled).ok_or(ImageReject::Undecodable)?;
    Ok(PreparedImage {
        mime: "image/jpeg",
        bytes: Cow::Owned(encoded),
    })
}

/// 原字节透传结论。
fn borrowed<'a>(mime: &'static str, bytes: &'a [u8]) -> PreparedImage<'a> {
    PreparedImage {
        mime,
        bytes: Cow::Borrowed(bytes),
    }
}

// =========================================================
// 格式识别
// =========================================================

/// 图片内容格式（按魔数识别；仅覆盖理解链路关心的格式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Jpeg,
    Png,
    Gif,
    Webp,
    Bmp,
}

impl ImageKind {
    /// 是否为线上后端接受的格式（其余需转换）。
    fn accepted_online(self) -> bool {
        !matches!(self, ImageKind::Bmp)
    }

    /// 声明的 MIME。
    fn mime(self) -> &'static str {
        match self {
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Png => "image/png",
            ImageKind::Gif => "image/gif",
            ImageKind::Webp => "image/webp",
            ImageKind::Bmp => "image/bmp",
        }
    }
}

/// 按魔数识别内容格式（未知格式返回 None）。
///
/// 判定顺序:
/// - JPEG `FF D8 FF`；PNG `89 50 4E 47`；GIF `47 49 46`；
/// - WEBP `RIFF....WEBP`；BMP `42 4D`。
fn detect_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some(ImageKind::Png)
    } else if bytes.starts_with(b"GIF") {
        Some(ImageKind::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else if bytes.starts_with(b"BM") {
        Some(ImageKind::Bmp)
    } else {
        None
    }
}

/// 读取头部像素尺寸（仅 PNG / JPEG / GIF；其余格式走解码判定）。
fn header_dimensions(bytes: &[u8], kind: ImageKind) -> Option<(u32, u32)> {
    match kind {
        ImageKind::Png => png_dimensions(bytes),
        ImageKind::Jpeg => jpeg_dimensions(bytes),
        ImageKind::Gif => gif_dimensions(bytes),
        ImageKind::Webp | ImageKind::Bmp => None,
    }
}

/// PNG：IHDR 块内宽高（大端，偏移 16 / 20）。
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// GIF：逻辑屏幕宽高（小端，偏移 6 / 8）。
fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 {
        return None;
    }
    let width = u32::from(u16::from_le_bytes(bytes[6..8].try_into().ok()?));
    let height = u32::from(u16::from_le_bytes(bytes[8..10].try_into().ok()?));
    Some((width, height))
}

/// JPEG：扫段定位 SOF 帧头（跳过不承载尺寸的段；未找到返回 None）。
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut index = 2;
    while index + 9 <= bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        let marker = bytes[index + 1];
        if marker == 0xFF {
            // 填充字节：后移一位重读
            index += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) {
            // 无载荷标记：仅标记本身
            index += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            // 文件结束 / 图像数据起始：未统计到 SOF
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]));
        if length < 2 {
            return None;
        }
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            // SOF：精度(1) + 高(2) + 宽(2)
            let height = u32::from(u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]));
            let width = u32::from(u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]));
            return Some((width, height));
        }
        index += 2 + length;
    }
    None
}

// =========================================================
// MIME 口径
// =========================================================

/// 按扩展名返回图片 MIME（未知扩展名回退 image/jpeg）。
pub(crate) fn image_mime_for(source_ref: &str) -> &'static str {
    let extension = Path::new(source_ref)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

/// 按魔数返回图片 MIME（未知格式回退 image/jpeg）。
pub(crate) fn image_mime_from_bytes(bytes: &[u8]) -> &'static str {
    detect_kind(bytes)
        .map(ImageKind::mime)
        .unwrap_or("image/jpeg")
}

/// 按魔数返回图片 MIME（未知格式返回 None，不做回退）。
///
/// 用途:
/// - 需要区分「未知格式」与「JPEG」的调用方（如图片入库前的格式校验）。
pub(crate) fn image_mime_checked(bytes: &[u8]) -> Option<&'static str> {
    detect_kind(bytes).map(ImageKind::mime)
}

// =========================================================
// 解码与重编码
// =========================================================

/// 受限解码（超出内存上限或解码失败返回 None）。
fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    // `Limits` 为 non_exhaustive：仅能按字段赋值
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(DECODE_MEMORY_LIMIT);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    reader.decode().ok()
}

/// 等比缩放到单边上限内（未越界原样返回）。
fn fit_within(image: DynamicImage, max_side: u32) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    if longest <= max_side {
        return image;
    }
    let scale = f64::from(max_side) / f64::from(longest);
    let target_width = ((f64::from(width) * scale).round() as u32).clamp(1, max_side);
    let target_height = ((f64::from(height) * scale).round() as u32).clamp(1, max_side);
    image.resize_exact(target_width, target_height, FilterType::Lanczos3)
}

/// 编码为 JPEG（含 alpha 时先合成到白底，避免透明区域变黑）。
fn encode_jpeg(image: &DynamicImage) -> Option<Vec<u8>> {
    let rgb = match image.color().has_alpha() {
        true => flatten_on_white(image),
        false => image.to_rgb8(),
    };
    let mut buffer = Vec::new();
    JpegEncoder::new_with_quality(&mut buffer, RESIZE_JPEG_QUALITY)
        .encode_image(&rgb)
        .ok()?;
    Some(buffer)
}

/// 合成到白底（按 alpha 混合，保留原色亮度）。
fn flatten_on_white(image: &DynamicImage) -> image::RgbImage {
    let rgba = image.to_rgba8();
    let mut base =
        image::RgbImage::from_pixel(rgba.width(), rgba.height(), image::Rgb([u8::MAX; 3]));
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let [red, green, blue, alpha] = pixel.0;
        let blend = |channel: u8| -> u8 {
            let alpha = u32::from(alpha);
            ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        base.put_pixel(x, y, image::Rgb([blend(red), blend(green), blend(blue)]));
    }
    base
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
