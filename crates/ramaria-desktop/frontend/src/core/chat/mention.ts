/**
 * src/core/chat/mention.ts — 正文 @ 提及切分（纯函数）
 *
 * 职责:
 * - 把正文拆为普通文本段与 @ 提及段，供气泡内高亮 chip 渲染
 *
 * 设计特点:
 * - 仅在明确需要高亮的语境启用（社交会话），本地会话不改变正文观感
 * - 提及体截断在空格与标点之前，避免把整句吞进 chip
 * - 不解析 Markdown，也不改写正文内容
 */

/** 内联分段：文本或提及 */
export type InlineSegment = { type: 'text' | 'mention'; value: string }

/** 提及形态：@ 加 1~20 个中英文、数字、下划线与连字符 */
const MENTION_PATTERN = /@[\u4e00-\u9fa5A-Za-z0-9_-]{1,20}/g

/**
 * 按 @ 提及切分正文。
 *
 * 参数:
 * - `text`: 正文
 * - `enabled`: 是否启用高亮（关闭时返回单段文本）
 *
 * 返回:
 * - 分段数组（可能为空数组表示空正文）
 */
export function splitMentions(text: string, enabled = true): InlineSegment[] {
  if (!text) {
    return []
  }
  if (!enabled) {
    return [{ type: 'text', value: text }]
  }

  const pattern = new RegExp(MENTION_PATTERN.source, 'g')
  const segments: InlineSegment[] = []
  let lastIndex = 0
  let match = pattern.exec(text)

  while (match !== null) {
    if (match.index > lastIndex) {
      segments.push({ type: 'text', value: text.slice(lastIndex, match.index) })
    }
    segments.push({ type: 'mention', value: match[0] })
    lastIndex = pattern.lastIndex
    match = pattern.exec(text)
  }

  if (lastIndex < text.length) {
    segments.push({ type: 'text', value: text.slice(lastIndex) })
  }

  return segments
}
