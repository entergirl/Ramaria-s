/**
 * tests/configDraft.test.ts — 配置草稿纯函数
 *
 * 覆盖:
 * - 路径读写（嵌套 / 缺失 / 不可变）
 * - 深比较与脏键计算
 * - 按组合并（保留目标文档中非本组字段）与按组恢复默认
 */

import { describe, expect, it } from 'vitest'

import {
  applyDefaults,
  cloneDocument,
  dirtyFieldKeys,
  jsonEqual,
  mergeFields,
  readPath,
  snapshotFields,
  writePath,
} from '../src/config/configDraft'
import type { ConfigDocument } from '../src/config/configTypes'
import { fieldsOfGroup } from '../src/config/settingsSchema'

function sampleDocument(): ConfigDocument {
  return {
    backend: { provider: 'lm_studio', base_url: 'http://localhost:1234/v1', temperature: 0.3 },
    session: { l1_idle_minutes: 10 },
    proactive: { enabled: true },
    l1: { progressive: { enabled: true } },
  }
}

describe('配置路径', () => {
  it('读取嵌套路径，缺失返回 undefined', () => {
    const document = sampleDocument()
    expect(readPath(document, ['backend', 'base_url'])).toBe('http://localhost:1234/v1')
    expect(readPath(document, ['l1', 'progressive', 'enabled'])).toBe(true)
    expect(readPath(document, ['backend', 'missing'])).toBeUndefined()
    expect(readPath(document, ['missing', 'deep'])).toBeUndefined()
  })

  it('写入返回新文档且不动原值', () => {
    const document = sampleDocument()
    const next = writePath(document, ['session', 'l1_idle_minutes'], 30)
    expect(readPath(next, ['session', 'l1_idle_minutes'])).toBe(30)
    expect(readPath(document, ['session', 'l1_idle_minutes'])).toBe(10)
  })

  it('写入缺失的中间节点时自动建对象', () => {
    const document = sampleDocument()
    const next = writePath(document, ['retrieval', 'rrf_k'], 60)
    expect(readPath(next, ['retrieval', 'rrf_k'])).toBe(60)
  })
})

describe('比较与脏值', () => {
  it('深比较覆盖数组与对象', () => {
    expect(jsonEqual([1, 2], [1, 2])).toBe(true)
    expect(jsonEqual([1, 2], [2, 1])).toBe(false)
    expect(jsonEqual({ a: 1 }, { a: 1, b: 2 })).toBe(false)
    expect(jsonEqual(undefined, undefined)).toBe(true)
    expect(jsonEqual(null, undefined)).toBe(false)
  })

  it('脏键按字段声明计算', () => {
    const baselineDocument = sampleDocument()
    const fields = fieldsOfGroup('session')
    const baseline = snapshotFields(baselineDocument, fields)
    const draft = writePath(baselineDocument, ['session', 'l1_idle_minutes'], 45)

    expect(dirtyFieldKeys(draft, baseline, fields)).toEqual(['session.l1_idle_minutes'])
    expect(dirtyFieldKeys(baselineDocument, baseline, fields)).toEqual([])
  })
})

describe('按组合并与恢复默认', () => {
  it('只覆盖本组字段，保留目标文档的其它组', () => {
    const draft = writePath(sampleDocument(), ['backend', 'base_url'], 'http://127.0.0.1:8000/v1')
    const latest = cloneDocument(sampleDocument())
    latest.session = { l1_idle_minutes: 99 }
    latest.proactive = { enabled: false }

    const merged = mergeFields(latest, draft, fieldsOfGroup('backend'))
    expect(readPath(merged, ['backend', 'base_url'])).toBe('http://127.0.0.1:8000/v1')
    expect(readPath(merged, ['session', 'l1_idle_minutes'])).toBe(99)
    expect(readPath(merged, ['proactive', 'enabled'])).toBe(false)
  })

  it('恢复默认只改字段集合内的键', () => {
    const draft = writePath(sampleDocument(), ['session', 'l1_idle_minutes'], 45)
    const defaults = sampleDocument()
    const next = applyDefaults(draft, defaults, fieldsOfGroup('session'))
    expect(readPath(next, ['session', 'l1_idle_minutes'])).toBe(10)
    expect(readPath(next, ['backend', 'base_url'])).toBe('http://localhost:1234/v1')
  })
})
