/**
 * tests/chatContent.test.ts — 消息正文纯逻辑
 *
 * 覆盖:
 * - `||` 拆条与流式展示的豁免区（代码块 / 行内代码 / 链接 / 裸 URL）
 * - 图片占位符三种形态的分段与序号
 * - @ 提及切分
 * - 图片 MIME 魔数嗅探
 */
import { describe, expect, it } from 'vitest'

import { splitBubbles, streamDisplay } from '../src/core/chat/bubble'
import {
  countImageSegments,
  splitImageSegments,
  sniffImageMime,
} from '../src/core/chat/imageText'
import { splitMentions } from '../src/core/chat/mention'

describe('splitBubbles', () => {
  it('按分隔符拆条并去掉段两侧空白', () => {
    expect(splitBubbles(' 先去吃饭 || 饿着扛可不行 ')).toEqual(['先去吃饭', '饿着扛可不行'])
  })

  it('无分隔符时为单段，空内容与纯分隔符返回空数组', () => {
    expect(splitBubbles('只有一句')).toEqual(['只有一句'])
    expect(splitBubbles('')).toEqual([])
    expect(splitBubbles('||')).toEqual([])
    expect(splitBubbles('  ||  ')).toEqual([])
  })

  it('围栏代码块内的分隔符不切分', () => {
    const content = '看这段\n```\na||b\n```\n||后面'
    expect(splitBubbles(content)).toEqual(['看这段\n```\na||b\n```', '后面'])
  })

  it('行内代码内的分隔符不切分', () => {
    expect(splitBubbles('`a||b`||下一段')).toEqual(['`a||b`', '下一段'])
  })

  it('裸 URL 内的分隔符不切分', () => {
    expect(splitBubbles('见 https://a.com/x||y ||结束')).toEqual([
      '见 https://a.com/x||y',
      '结束',
    ])
  })

  it('链接目标内的分隔符不切分', () => {
    expect(splitBubbles('[说明](https://a.com/a||b)||下一段')).toEqual([
      '[说明](https://a.com/a||b)',
      '下一段',
    ])
  })

  it('流式展示把分隔符换成换行，且保留豁免区', () => {
    expect(streamDisplay('a||b')).toBe('a\nb')
    expect(streamDisplay('`a||b`')).toBe('`a||b`')
    expect(streamDisplay('')).toBe('')
  })
})

describe('splitImageSegments', () => {
  it('识别三种占位符并累计序号', () => {
    const segments = splitImageSegments('前[图片#abcd1234]中[图片: 一只猫]后[图片]')
    expect(segments.map((segment) => segment.type)).toEqual([
      'text',
      'image',
      'text',
      'image',
      'text',
      'image',
    ])
    const images = segments.filter((segment) => segment.type === 'image')
    expect(images.map((segment) => segment.index)).toEqual([0, 1, 2])
    expect(images[0]).toMatchObject({ hash: 'abcd1234', desc: '' })
    expect(images[1]).toMatchObject({ hash: '', desc: '一只猫' })
    expect(images[2]).toMatchObject({ hash: '', desc: '' })
  })

  it('位数不符或未闭合的占位符按普通文本处理', () => {
    const segments = splitImageSegments('[图片#abcd] 与 [图片: 未闭合')
    expect(segments).toHaveLength(1)
    expect(segments[0].type).toBe('text')
    expect(countImageSegments('[图片#abcd] 与 [图片: 未闭合')).toBe(0)
  })
})

describe('splitMentions', () => {
  it('启用时把 @ 提及单独成段', () => {
    expect(splitMentions('你好 @小珊 看这个')).toEqual([
      { type: 'text', value: '你好 ' },
      { type: 'mention', value: '@小珊' },
      { type: 'text', value: ' 看这个' },
    ])
  })

  it('关闭时原样单段', () => {
    expect(splitMentions('你好 @小珊', false)).toEqual([{ type: 'text', value: '你好 @小珊' }])
  })

  it('独立 @ 不构成提及', () => {
    expect(splitMentions('价格 @ 100')).toEqual([{ type: 'text', value: '价格 @ 100' }])
  })
})

describe('sniffImageMime', () => {
  const buffer = (bytes: number[]): ArrayBuffer => new Uint8Array(bytes).buffer

  it('按魔数识别五类图片', () => {
    expect(sniffImageMime(buffer([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))).toBe(
      'image/png',
    )
    expect(sniffImageMime(buffer([0xff, 0xd8, 0xff, 0xe0]))).toBe('image/jpeg')
    expect(sniffImageMime(buffer([0x47, 0x49, 0x46, 0x38, 0x39]))).toBe('image/gif')
    expect(
      sniffImageMime(
        buffer([0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50]),
      ),
    ).toBe('image/webp')
    expect(sniffImageMime(buffer([0x42, 0x4d, 0x00]))).toBe('image/bmp')
  })

  it('无法识别时回退二进制类型', () => {
    expect(sniffImageMime(buffer([0x00, 0x01]))).toBe('application/octet-stream')
  })
})
