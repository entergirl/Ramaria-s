//! crates/ramaria-desktop/src/tray_badge.rs - Ramaria 托盘未读徽标绘制
//!
//! 设计特点:
//! - 纯函数像素合成：在 RGBA 缓冲区右上角绘制深底白字未读徽标，便于单测锁定
//! - 徽标底色取与图标底色的高对比深色（图标为品牌红，同色系徽标在 16px 托盘尺寸下不可辨）
//! - 数字采用 3×5 点阵、2 倍缩放；最多两位，超过 99 显示 99
//! - 徽标与图标上 / 右边缘各留 1 像素，避免与图标轮廓粘连
//! - 0 不绘制（调用方直接使用基础像素）；徽标放不下时不绘制（防御小图标）

// =========================================================
// 绘制参数
// =========================================================

/// 数字点阵（3×5；每行按 bit2→bit0 从左到右对应三列）。
const DIGIT_BITMAPS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// 点阵数字宽 / 高（点阵格数）。
const GLYPH_COLS: u32 = 3;
const GLYPH_ROWS: u32 = 5;
/// 点阵单格缩放倍数（1 格放大为 N×N 像素）。
const DIGIT_SCALE: u32 = 2;
/// 两位数字之间的间距（像素）。
const DIGIT_GAP: u32 = 2;
/// 徽标内边距（横向 / 纵向，像素）。
const BADGE_PAD_X: u32 = 2;
const BADGE_PAD_Y: u32 = 1;
/// 徽标距图标上 / 右边缘的距离（像素）。
const BADGE_MARGIN_TOP: u32 = 1;
const BADGE_MARGIN_RIGHT: u32 = 1;
/// 徽标底色（深色）：与品牌红图标形成高对比，且白字在其上缩放后仍清晰
/// （白底 / 描边方案在 16px 托盘缩放下会与图标底色互相渗透）。
const BADGE_BG: [u8; 3] = [32, 33, 36];
/// 数字颜色。
const BADGE_FG: [u8; 3] = [255, 255, 255];

// =========================================================
// 像素合成
// =========================================================

/// 在 RGBA 像素缓冲区右上角合成未读徽标。
///
/// 参数:
/// - `rgba`: 图标像素缓冲（RGBA 排列，原地修改）。
/// - `width`: 图标宽度（像素；高度由缓冲区长度与宽度推算）。
/// - `unread`: 未读消息数（0 不绘制；>=100 显示 99）。
///
/// 说明:
/// - 数字右对齐、距上边缘 [`BADGE_MARGIN_TOP`] 像素、距右边缘 [`BADGE_MARGIN_RIGHT`] 像素；
/// - 徽标尺寸超出图标时不绘制（防御性，正常 32×32 图标可容纳两位数字）。
pub(crate) fn compose_badge_rgba(rgba: &mut [u8], width: u32, unread: u32) {
    if unread == 0 || width == 0 {
        return;
    }
    let height = rgba.len() as u32 / (width * 4);
    if height == 0 {
        return;
    }

    let text = if unread >= 100 {
        "99".to_string()
    } else {
        unread.to_string()
    };
    let digits: Vec<u8> = text.bytes().map(|b| b - b'0').collect();
    let glyph_w = GLYPH_COLS * DIGIT_SCALE;
    let glyph_h = GLYPH_ROWS * DIGIT_SCALE;
    let digit_count = digits.len() as u32;
    let text_w = digit_count * glyph_w + digit_count.saturating_sub(1) * DIGIT_GAP;
    let badge_w = text_w + 2 * BADGE_PAD_X;
    let badge_h = glyph_h + 2 * BADGE_PAD_Y;

    if badge_w + BADGE_MARGIN_RIGHT > width || badge_h + BADGE_MARGIN_TOP > height {
        return;
    }

    let x0 = width - badge_w - BADGE_MARGIN_RIGHT;
    let y0 = BADGE_MARGIN_TOP;

    // 高对比底
    for y in y0..y0 + badge_h {
        for x in x0..x0 + badge_w {
            set_pixel(rgba, width, height, x, y, BADGE_BG);
        }
    }

    // 数字：逐数字按点阵逐格放大绘制
    for (index, &digit) in digits.iter().enumerate() {
        let gx = x0 + BADGE_PAD_X + index as u32 * (glyph_w + DIGIT_GAP);
        let gy = y0 + BADGE_PAD_Y;
        let bitmap = &DIGIT_BITMAPS[digit as usize];
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..GLYPH_COLS {
                if *bits & (1 << (GLYPH_COLS - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..DIGIT_SCALE {
                    for dx in 0..DIGIT_SCALE {
                        let px = gx + col * DIGIT_SCALE + dx;
                        let py = gy + row as u32 * DIGIT_SCALE + dy;
                        set_pixel(rgba, width, height, px, py, BADGE_FG);
                    }
                }
            }
        }
    }
}

/// 写入单个像素（越界静默跳过；alpha 恒为不透明）。
fn set_pixel(rgba: &mut [u8], width: u32, height: u32, x: u32, y: u32, rgb: [u8; 3]) {
    if x >= width || y >= height {
        return;
    }
    let index = ((y * width + x) * 4) as usize;
    if index + 3 >= rgba.len() {
        return;
    }
    rgba[index] = rgb[0];
    rgba[index + 1] = rgb[1];
    rgba[index + 2] = rgb[2];
    rgba[index + 3] = 0xff;
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::TRAY_ICON_RGBA;

    /// 构造 32×32 纯色基础图标缓冲。
    fn base_pixels() -> Vec<u8> {
        let mut pixels = vec![0u8; (32 * 32 * 4) as usize];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&TRAY_ICON_RGBA);
        }
        pixels
    }

    /// 统计指定颜色像素数。
    fn count_color(pixels: &[u8], rgb: [u8; 3]) -> usize {
        pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == rgb)
            .count()
    }

    /// 指定颜色像素的 x 坐标跨度（最大值 - 最小值）。
    fn x_span(pixels: &[u8], rgb: [u8; 3]) -> u32 {
        let xs: Vec<u32> = pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, pixel)| pixel[..3] == rgb)
            .map(|(index, _)| index as u32 % 32)
            .collect();
        let min = xs.iter().min().expect("应有目标颜色像素");
        let max = xs.iter().max().expect("应有目标颜色像素");
        max - min
    }

    /// 感知亮度（Rec.601 加权，归一到 0..1）。
    fn luminance(rgb: [u8; 3]) -> f64 {
        (0.299 * f64::from(rgb[0]) + 0.587 * f64::from(rgb[1]) + 0.114 * f64::from(rgb[2])) / 255.0
    }

    /// unread=0：不绘制任何徽标像素（缓冲区逐字节不变）。
    #[test]
    fn zero_unread_draws_nothing() {
        let mut pixels = base_pixels();
        let before = pixels.clone();
        compose_badge_rgba(&mut pixels, 32, 0);
        assert_eq!(pixels, before, "无未读不应改动像素");
    }

    /// 单位数：右上角出现徽标底与数字，且徽标整体位于右半区、与右边缘留边。
    #[test]
    fn single_digit_draws_badge_top_right() {
        let mut pixels = base_pixels();
        compose_badge_rgba(&mut pixels, 32, 3);
        assert!(count_color(&pixels, BADGE_BG) > 0, "应出现徽标底像素");
        assert!(count_color(&pixels, BADGE_FG) > 0, "应出现数字像素");
        let xs: Vec<u32> = pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, pixel)| pixel[..3] == BADGE_BG)
            .map(|(index, _)| index as u32 % 32)
            .collect();
        let min_x = *xs.iter().min().expect("应有徽标底像素");
        let max_x = *xs.iter().max().expect("应有徽标底像素");
        assert!(min_x >= 16, "徽标应右对齐: min_x={min_x}");
        assert_eq!(
            max_x,
            32 - 1 - BADGE_MARGIN_RIGHT,
            "徽标应与图标右边缘留出间距"
        );
    }

    /// 徽标底色与图标底色必须有足够亮度差
    /// （回归守卫：同色系徽标在 16px 托盘尺寸下与图标糊在一起，只剩数字可见；
    /// 实测旧版"红底 + 品牌红图标"亮度差仅 0.05）。
    #[test]
    fn badge_background_contrasts_with_icon() {
        let icon = [TRAY_ICON_RGBA[0], TRAY_ICON_RGBA[1], TRAY_ICON_RGBA[2]];
        let delta = (luminance(BADGE_BG) - luminance(icon)).abs();
        assert!(
            delta >= 0.25,
            "徽标底色与图标底色亮度差过小: bg={BADGE_BG:?} icon={icon:?} delta={delta:.2}"
        );
    }

    /// 两位数字：数字横向跨度大于单位数（间距与逐位绘制生效）。
    #[test]
    fn two_digits_are_wider_than_one() {
        let mut one = base_pixels();
        compose_badge_rgba(&mut one, 32, 5);
        let mut two = base_pixels();
        compose_badge_rgba(&mut two, 32, 25);
        assert_eq!(x_span(&one, BADGE_FG), 5, "单位数点阵缩 2 倍宽 6 → 跨度 5");
        assert_eq!(x_span(&two, BADGE_FG), 13, "两位 = 6 + 2 + 6 → 跨度 13");
    }

    /// 溢出截断：>=100 与 99 的像素完全一致。
    #[test]
    fn over_99_shows_99() {
        let mut hundred = base_pixels();
        compose_badge_rgba(&mut hundred, 32, 100);
        let mut ninety_nine = base_pixels();
        compose_badge_rgba(&mut ninety_nine, 32, 99);
        assert_eq!(hundred, ninety_nine, ">=100 应显示 99");
    }

    /// 小图标放不下徽标时不绘制（防御性）。
    #[test]
    fn small_icon_skips_badge() {
        let mut pixels = vec![0u8; (8 * 8 * 4) as usize];
        compose_badge_rgba(&mut pixels, 8, 99);
        assert_eq!(count_color(&pixels, BADGE_BG), 0, "放不下时不绘制");
    }
}
