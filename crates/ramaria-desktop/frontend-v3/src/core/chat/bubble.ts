/**
 * src/core/chat/bubble.ts — 回复多气泡切分（纯函数）
 *
 * 职责:
 * - 把助手回复文本按分隔符 `||` 切分为多条气泡文本
 * - 为流式渲染提供「分隔符临时显示为换行」的同口径文本
 *
 * 设计特点:
 * - 只做切分与清洗（trim、丢弃空段），不渲染、不触碰 DOM
 * - 围栏代码块、行内代码、Markdown 链接目标与裸 URL 内部的 `||` 属于正文，不参与切分
 * - 无有效分段时返回空数组，由调用方回退「原文单泡」，不产生空气泡、不丢内容
 * - 单遍扫描状态机，不回溯
 */

/** 多气泡分隔符（与提示词「核心规则」一致） */
export const BUBBLE_SEPARATOR = '||'

/** 裸 URL 判定前缀（命中后整段吞掉，内部 `||` 不切分） */
const URL_PREFIX = /^(https?:\/\/|www\.|mailto:)/i

/**
 * 裸 URL 终止字符（空白、引号、右括号与中文标点）。
 *
 * 说明:
 * - 不把 ASCII `:` / `;` / `?` / `!` 列为终止符——它们是 URL 的合法字符
 */
const URL_TERMINATOR = /[\s<>"'`)\]）】}，。、！？；：]/

/** 统计从下标 `i` 起、由同一字符 `ch` 组成的连续长度 */
function countRun(content: string, i: number, ch: string): number {
  let j = i
  while (j < content.length && content.charAt(j) === ch) {
    j += 1
  }
  return j - i
}

/** 生成 `count` 个 `ch` 组成的字符串 */
function repeatChar(ch: string, count: number): string {
  return ch.repeat(count)
}

/** 判断下标 `i` 处是否为裸 URL 起点 */
function isUrlStart(content: string, i: number): boolean {
  return URL_PREFIX.test(content.slice(i, i + 16))
}

/**
 * 按 `||` 切分原文，返回未清洗的原始段数组。
 *
 * 参数:
 * - `content`: 待扫描的原文
 *
 * 返回:
 * - 原始段数组（至少 1 段；分隔符本身不保留）
 */
function splitRaw(content: string): string[] {
  const segments: string[] = []
  let buffer = ''
  let i = 0
  const total = content.length
  let fence = ''
  let inline = ''
  let atLineStart = true

  while (i < total) {
    const ch = content.charAt(i)

    // 围栏代码块内部：只识别行首闭合标记，其余字符原样复制
    if (fence !== '') {
      if (atLineStart && ch === fence.charAt(0)) {
        const closeRun = countRun(content, i, ch)
        if (closeRun >= fence.length) {
          fence = ''
          buffer += content.slice(i, i + closeRun)
          i += closeRun
          atLineStart = false
          continue
        }
      }
      buffer += ch
      atLineStart = ch === '\n'
      i += 1
      continue
    }

    // 围栏代码块开启：行首 3 个以上同字符（反引号或波浪线）
    if (atLineStart && (ch === '`' || ch === '~')) {
      const openRun = countRun(content, i, ch)
      if (openRun >= 3) {
        fence = repeatChar(ch, openRun)
        buffer += content.slice(i, i + openRun)
        i += openRun
        atLineStart = false
        continue
      }
    }

    // 行内代码：同长度反引号串开启与闭合
    if (ch === '`') {
      const tickRun = countRun(content, i, '`')
      if (inline === '') {
        inline = repeatChar('`', tickRun)
      } else if (tickRun === inline.length) {
        inline = ''
      }
      buffer += content.slice(i, i + tickRun)
      i += tickRun
      atLineStart = false
      continue
    }
    if (inline !== '') {
      buffer += ch
      atLineStart = ch === '\n'
      i += 1
      continue
    }

    // Markdown 链接目标：整段吞掉（含嵌套括号）
    if (ch === ']' && content.charAt(i + 1) === '(') {
      buffer += ']('
      i += 2
      let depth = 1
      while (i < total && depth > 0) {
        const inner = content.charAt(i)
        if (inner === '(') {
          depth += 1
        } else if (inner === ')') {
          depth -= 1
        }
        buffer += inner
        i += 1
      }
      atLineStart = false
      continue
    }

    // 裸 URL：整段吞掉
    if (isUrlStart(content, i)) {
      while (i < total && !URL_TERMINATOR.test(content.charAt(i))) {
        buffer += content.charAt(i)
        i += 1
      }
      atLineStart = false
      continue
    }

    // 分隔符：仅在豁免区之外切分
    if (ch === '|' && content.charAt(i + 1) === '|') {
      segments.push(buffer)
      buffer = ''
      i += 2
      atLineStart = false
      continue
    }

    buffer += ch
    if (ch === '\n') {
      atLineStart = true
    } else if (!(atLineStart && (ch === ' ' || ch === '\t'))) {
      atLineStart = false
    }
    i += 1
  }

  segments.push(buffer)
  return segments
}

/**
 * 将回复文本切分为多条气泡文本。
 *
 * 参数:
 * - `content`: 回复原文（可能含分隔符）
 *
 * 返回:
 * - 非空段数组（每段已 trim）；无分隔符时为单段；
 *   空串或全分隔符返回空数组，由调用方回退原文单泡
 */
export function splitBubbles(content: string): string[] {
  if (!content) {
    return []
  }
  return splitRaw(content)
    .map((segment) => segment.trim())
    .filter((segment) => segment !== '')
}

/**
 * 流式展示文本：把豁免区之外的 `||` 替换为换行。
 *
 * 参数:
 * - `content`: 已累积的回复文本
 *
 * 返回:
 * - 展示文本；空串原样返回
 *
 * 说明:
 * - 不 trim，避免逐字渲染时抖动
 */
export function streamDisplay(content: string): string {
  if (!content) {
    return ''
  }
  return splitRaw(content).join('\n')
}
