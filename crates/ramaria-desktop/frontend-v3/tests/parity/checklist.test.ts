/**
 * tests/parity/checklist.test.ts — parity 清单守卫
 *
 * 覆盖:
 * - 清单结构：四视图齐全、条目编号唯一、维度合法、覆盖与不适用各带必填字段
 * - 覆盖引用真实存在：条目指到的测试文件里的用例名没有被改名或删除
 * - 与 `design-qa.md` §9 对齐：格名集合一致、`[x]` 对应已覆盖、`[-]` 对应不适用
 *
 * 说明:
 * - 两份外部文本经 `import.meta.glob` 的 `?raw` 导入取得，避免引入 Node 类型依赖
 */

import { describe, expect, it } from 'vitest'

import { PARITY_CHECKLIST, type ParityEntry } from './checklist'

/** 测试目录下所有测试文件源码，键为 vite 解析后的相对路径 */
const TEST_SOURCES = import.meta.glob('/tests/**/*.test.ts', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

const QA_SOURCES = import.meta.glob('/../../../../docs/dev-3.0/test/design-qa.md', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

const DIMENSIONS = ['content', 'operation', 'state']

interface QaCell {
  cell: string
  mark: string
}

function sourceBySuffix(sources: Record<string, string>, suffix: string): string {
  const hit = Object.entries(sources).find(([key]) => key.endsWith(suffix))
  if (!hit) {
    throw new Error(`未取得源文本: ${suffix}`)
  }
  return hit[1]
}

/** 从设计 QA 文档解析出某视图的验收格 */
function qaCells(view: string): QaCell[] {
  const rows = sourceBySuffix(QA_SOURCES, 'design-qa.md').split('\n')
  const row = rows.find((line) => {
    const columns = line.split('|')
    return columns[1]?.trim() === `QA-V-${view}` && columns[2]?.trim() === view
  })
  if (!row) {
    throw new Error(`设计 QA 文档缺少 QA-V-${view} 行`)
  }
  const field: string = row.split('|')[3] ?? ''
  return Array.from(field.matchAll(/\[([ x-])\]([^\s|]+)/g)).map((match: RegExpMatchArray) => ({
    mark: match[1] ?? '',
    cell: match[2] ?? '',
  }))
}

function stateEntries(view: string): ParityEntry[] {
  const hit = PARITY_CHECKLIST.find((item) => item.view === view)
  if (!hit) {
    throw new Error(`清单缺少视图 ${view}`)
  }
  return hit.entries.filter((entry) => entry.dimension === 'state')
}

function caseNamesOf(file: string): string[] {
  const source = sourceBySuffix(TEST_SOURCES, file)
  return Array.from(source.matchAll(/it\(\s*['"`]([^'"`]+)['"`]/g)).map(
    (match: RegExpMatchArray) => match[1] ?? '',
  )
}

describe('parity 清单结构', () => {
  it('五视图齐全且顺序固定', () => {
    expect(PARITY_CHECKLIST.map((item) => item.view)).toEqual([
      'memory',
      'persona',
      'rules',
      'import',
      'debug',
    ])
  })

  it('条目编号唯一、维度合法，已覆盖与不适用各带必填字段', () => {
    const ids = new Set<string>()
    for (const view of PARITY_CHECKLIST) {
      expect(view.entries.length).toBeGreaterThan(0)
      for (const entry of view.entries) {
        expect(ids.has(entry.id), `编号重复: ${entry.id}`).toBe(false)
        ids.add(entry.id)
        expect(entry.id.startsWith(`${view.view}-${entry.dimension}-`)).toBe(true)
        expect(DIMENSIONS).toContain(entry.dimension)
        expect(entry.claim.length).toBeGreaterThan(0)
        if (entry.status === 'covered') {
          expect(entry.coverage?.file, `${entry.id} 缺覆盖文件`).toBeTruthy()
          expect(entry.coverage?.cases.length ?? 0).toBeGreaterThan(0)
          expect(entry.naReason).toBeUndefined()
        } else {
          expect(entry.naReason, `${entry.id} 缺不适用依据`).toBeTruthy()
          expect(entry.coverage).toBeUndefined()
        }
        expect(entry.cell === undefined).toBe(entry.dimension !== 'state')
      }
    }
  })

  it('覆盖引用指向真实存在的用例', () => {
    const names = new Map<string, string[]>()
    for (const view of PARITY_CHECKLIST) {
      for (const entry of view.entries) {
        const coverage = entry.coverage
        if (!coverage) {
          continue
        }
        if (!names.has(coverage.file)) {
          names.set(coverage.file, caseNamesOf(coverage.file))
        }
        const declared = names.get(coverage.file) ?? []
        for (const name of coverage.cases) {
          expect(declared, `${coverage.file} 缺少用例「${name}」`).toContain(name)
        }
      }
    }
  })
})

describe('parity 清单与设计 QA 对齐', () => {
  for (const view of PARITY_CHECKLIST) {
    it(`${view.qaId} 的格名与标记和文档一致`, () => {
      const doc = qaCells(view.view)
      const list = stateEntries(view.view)
      expect(list.map((entry) => entry.cell)).toEqual(doc.map((item) => item.cell))

      for (const item of doc) {
        const entry = list.find((candidate) => candidate.cell === item.cell)
        if (!entry) {
          throw new Error(`${view.qaId} 清单缺少格 ${item.cell}`)
        }
        const expected = entry.status === 'na' ? '-' : 'x'
        expect(item.mark, `${view.qaId} 的 ${item.cell} 在文档里应为 [${expected}]`).toBe(expected)
      }
    })
  }

  it('五个视图的状态格合计与文档一致', () => {
    const total = PARITY_CHECKLIST.reduce(
      (sum, view) => sum + view.entries.filter((entry) => entry.dimension === 'state').length,
      0,
    )
    expect(total).toBe(34)
  })
})
