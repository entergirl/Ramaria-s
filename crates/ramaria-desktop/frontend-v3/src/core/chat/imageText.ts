/**
 * src/core/chat/imageText.ts — 正文图片占位符解析（纯函数）
 *
 * 职责:
 * - 把消息正文按图片占位符切分为文本段与图片段
 * - 统计图片段数量，供跨气泡段的图片序号累计
 * - 按字节魔数嗅探图片 MIME，供前端构造 Blob URL
 *
 * 设计特点:
 * - 三种占位符形态按出现顺序识别：`[图片#hash]`（8 位 ASCII）/ `[图片: 描述]` / `[图片]`
 * - 其余写法（位数不符的 hash、描述含方括号、未闭合）按普通文本处理、不占图片序号
 * - 无文件形态不发读取请求，但仍占序号，保证序号与正文出现顺序一致
 */

/** 正文分段：文本或图片（`index` 为该段在单条文本内的序号，从 0 起） */
export type MessageSegment =
  | { type: 'text'; value: string }
  | { type: 'image'; index: number; hash: string; desc: string }

/** 占位符正则（每次调用新建，避免全局匹配的 lastIndex 串用） */
function imagePattern(): RegExp {
  return /\[图片#([\x00-\x7F]{8})\]|\[图片: ([^[\]]+)\]|\[图片\]/g
}

/**
 * 将文本按图片占位符切分为文本段与图片段。
 *
 * 参数:
 * - `text`: 气泡文本
 *
 * 返回:
 * - 分段数组；无占位符时返回单段文本
 */
export function splitImageSegments(text: string): MessageSegment[] {
  const source = typeof text === 'string' ? text : ''
  const pattern = imagePattern()
  const segments: MessageSegment[] = []
  let lastIndex = 0
  let imageIndex = 0
  let match = pattern.exec(source)

  while (match !== null) {
    if (match.index > lastIndex) {
      segments.push({ type: 'text', value: source.slice(lastIndex, match.index) })
    }
    segments.push({
      type: 'image',
      index: imageIndex,
      hash: match[1] ?? '',
      desc: match[2] ?? '',
    })
    imageIndex += 1
    lastIndex = pattern.lastIndex
    match = pattern.exec(source)
  }

  if (lastIndex < source.length) {
    segments.push({ type: 'text', value: source.slice(lastIndex) })
  }

  return segments
}

/** 统计文本中的图片段数量 */
export function countImageSegments(text: string): number {
  return splitImageSegments(text).filter((segment) => segment.type === 'image').length
}

/** 分段结果中是否含图片段 */
export function hasImageSegment(segments: MessageSegment[]): boolean {
  return segments.some((segment) => segment.type === 'image')
}

/**
 * 按字节魔数嗅探图片 MIME 类型。
 *
 * 参数:
 * - `bytes`: 图片字节
 *
 * 返回:
 * - 五类图片 MIME；无法识别时返回 `application/octet-stream`
 */
export function sniffImageMime(bytes: ArrayBuffer | ArrayLike<number>): string {
  let view: Uint8Array
  try {
    view =
      bytes instanceof ArrayBuffer ? new Uint8Array(bytes) : new Uint8Array(bytes as ArrayLike<number>)
  } catch {
    return 'application/octet-stream'
  }

  if (
    view.length >= 8 &&
    view[0] === 0x89 &&
    view[1] === 0x50 &&
    view[2] === 0x4e &&
    view[3] === 0x47 &&
    view[4] === 0x0d &&
    view[5] === 0x0a &&
    view[6] === 0x1a &&
    view[7] === 0x0a
  ) {
    return 'image/png'
  }
  if (view.length >= 3 && view[0] === 0xff && view[1] === 0xd8 && view[2] === 0xff) {
    return 'image/jpeg'
  }
  if (view.length >= 4 && view[0] === 0x47 && view[1] === 0x49 && view[2] === 0x46 && view[3] === 0x38) {
    return 'image/gif'
  }
  if (
    view.length >= 12 &&
    view[0] === 0x52 &&
    view[1] === 0x49 &&
    view[2] === 0x46 &&
    view[3] === 0x46 &&
    view[8] === 0x57 &&
    view[9] === 0x45 &&
    view[10] === 0x42 &&
    view[11] === 0x50
  ) {
    return 'image/webp'
  }
  if (view.length >= 2 && view[0] === 0x42 && view[1] === 0x4d) {
    return 'image/bmp'
  }
  return 'application/octet-stream'
}
