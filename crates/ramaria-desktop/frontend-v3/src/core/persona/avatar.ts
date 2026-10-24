/**
 * src/core/persona/avatar.ts — 头像文件处理
 *
 * 职责:
 * - 校验用户选中的图片文件，裁剪为正方形并压成 data URI
 *
 * 设计特点:
 * - 只接受图片且限制单文件体积，避免超大原图进内存
 * - 中心裁方形 + 定尺寸 + JPEG 压缩，控制写入 `avatar` 的字符串长度
 * - 透明区域填白，避免 PNG 转 JPEG 后出现黑底
 * - 不使用本地文件路径：页面 CSP 只允许 `data:` / `blob:` 图源
 */

/** 头像可选文件的 MIME 白名单 */
export const AVATAR_MIME = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'] as const

/** `accept` 属性取值 */
export const AVATAR_ACCEPT = AVATAR_MIME.join(',')

/** 单文件体积上限（字节） */
export const AVATAR_MAX_BYTES = 20 * 1024 * 1024

/** 输出边长（像素）；约等于 96px 展示尺寸的 2 倍图 */
export const AVATAR_SIZE = 192

const JPEG_QUALITY = 0.82

/**
 * 校验头像文件。
 *
 * 参数:
 * - `file`: 用户选中的文件（只需 `type` 与 `size`）。
 *
 * 返回:
 * - 空串表示可用；否则为面向用户的短句。
 */
export function avatarFileError(file: { type: string; size: number }): string {
  const allowed = AVATAR_MIME as readonly string[]
  if (!allowed.includes(file.type)) {
    return '请选择 PNG / JPEG / WebP / GIF 图片'
  }
  if (file.size > AVATAR_MAX_BYTES) {
    return '图片过大，请选择 20MB 以内的文件'
  }
  return ''
}

/**
 * 把图片文件转成方形头像 data URI。
 *
 * 参数:
 * - `file`: 已通过 `avatarFileError` 校验的图片文件。
 * - `size`: 输出边长，缺省 `AVATAR_SIZE`。
 *
 * 返回:
 * - `data:image/jpeg;base64,…` 字符串。
 *
 * 错误:
 * - 浏览器无法解码该图片，或画布不可用时抛错。
 */
export async function fileToAvatarDataUrl(
  file: Blob,
  size: number = AVATAR_SIZE,
): Promise<string> {
  const bitmap = await createImageBitmap(file)
  try {
    const side = Math.min(bitmap.width, bitmap.height)
    if (side <= 0) {
      throw new Error('图片尺寸无效')
    }
    const canvas = document.createElement('canvas')
    canvas.width = size
    canvas.height = size
    const context = canvas.getContext('2d')
    if (!context) {
      throw new Error('当前环境无法处理图片')
    }
    context.fillStyle = '#ffffff'
    context.fillRect(0, 0, size, size)
    context.drawImage(
      bitmap,
      (bitmap.width - side) / 2,
      (bitmap.height - side) / 2,
      side,
      side,
      0,
      0,
      size,
      size,
    )
    return canvas.toDataURL('image/jpeg', JPEG_QUALITY)
  } finally {
    bitmap.close()
  }
}
