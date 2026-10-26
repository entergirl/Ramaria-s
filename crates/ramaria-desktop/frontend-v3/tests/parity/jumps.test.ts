/**
 * tests/parity/jumps.test.ts — 跨视图跳转矩阵守卫
 *
 * 覆盖:
 * - 清单结构：编号唯一、触发与目标非空、覆盖与不适用各带必填字段
 * - 覆盖引用真实存在：条目指到的测试文件里的用例名没有被改名或删除
 * - 与 `frontend-spec-v2.md` §8 对齐：同编号行的触发 / 目标 / 参数逐字一致
 * - 参数 grep 兜底：跳转参数与一次性状态的含义在源码里有对应落点
 *
 * 说明:
 * - 外部文本经 `import.meta.glob` 的 `?raw` 导入取得，避免引入 Node 类型依赖
 */

import { describe, expect, it } from 'vitest'

import { JUMP_MATRIX } from './jumps'

const TEST_SOURCES = import.meta.glob('/tests/**/*.test.ts', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

const SPEC_SOURCES = import.meta.glob('/../../../../docs/dev-3.0/test/frontend-spec-v2.md', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

const SRC_SOURCES = import.meta.glob('/src/views/{memory,persona,import}/**/*.{ts,vue}', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

interface MatrixRow {
  no: string
  trigger: string
  target: string
  params: string
}

function sourceBySuffix(sources: Record<string, string>, suffix: string): string {
  const hit = Object.entries(sources).find(([key]) => key.endsWith(suffix))
  if (!hit) {
    throw new Error(`未取得源文本: ${suffix}`)
  }
  return hit[1]
}

/** 从规格 §8 的表格解析跳转矩阵行 */
function matrixRows(): MatrixRow[] {
  const lines = sourceBySuffix(SPEC_SOURCES, 'frontend-spec-v2.md').split('\n')
  const start = lines.findIndex((line) => line.trim() === '## 8. 跨视图跳转矩阵')
  if (start < 0) {
    throw new Error('规格文档缺少 §8 跳转矩阵小节')
  }

  const rows: MatrixRow[] = []
  for (const line of lines.slice(start)) {
    if (line.trim().startsWith('## ') && line.trim() !== '## 8. 跨视图跳转矩阵') {
      break
    }
    const columns = line.split('|').map((item) => item.trim())
    // 表格行形如 `| 7 | 触发 | 目标 | 参数 |`
    if (columns.length < 6 || !/^\d+$/.test(columns[1] ?? '')) {
      continue
    }
    rows.push({
      no: columns[1] ?? '',
      trigger: columns[2] ?? '',
      target: (columns[3] ?? '').replace(/`/g, ''),
      params: (columns[4] ?? '').replace(/`/g, ''),
    })
  }
  return rows
}

function rowOf(no: number): MatrixRow {
  const hit = matrixRows().find((row) => row.no === String(no))
  if (!hit) {
    throw new Error(`规格 §8 缺少第 ${no} 条跳转`)
  }
  return hit
}

function caseNamesOf(file: string): string[] {
  const source = sourceBySuffix(TEST_SOURCES, file)
  return Array.from(source.matchAll(/it\(\s*['"`]([^'"`]+)['"`]/g)).map(
    (match: RegExpMatchArray) => match[1] ?? '',
  )
}

describe('跳转矩阵清单结构', () => {
  it('编号唯一且字段完整', () => {
    const seen = new Set<number>()
    for (const entry of JUMP_MATRIX) {
      expect(seen.has(entry.no), `编号重复: ${entry.no}`).toBe(false)
      seen.add(entry.no)
      expect(entry.trigger.length, `第 ${entry.no} 条缺触发`).toBeGreaterThan(0)
      expect(entry.target.length, `第 ${entry.no} 条缺目标`).toBeGreaterThan(0)
      expect(entry.params.length, `第 ${entry.no} 条缺参数`).toBeGreaterThan(0)
      if (entry.status === 'covered') {
        expect(entry.coverage?.file, `第 ${entry.no} 条缺覆盖文件`).toBeTruthy()
        expect(entry.coverage?.cases.length ?? 0).toBeGreaterThan(0)
        expect(entry.naReason).toBeUndefined()
      } else {
        expect(entry.naReason, `第 ${entry.no} 条缺不适用依据`).toBeTruthy()
        expect(entry.coverage).toBeUndefined()
      }
    }
  })

  it('覆盖引用指向真实存在的用例', () => {
    const names = new Map<string, string[]>()
    for (const entry of JUMP_MATRIX) {
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
  })
})

describe('跳转矩阵与规格对齐', () => {
  for (const entry of JUMP_MATRIX) {
    it(`第 ${entry.no} 条与规格 §8 同编号行一致`, () => {
      const row = rowOf(entry.no)
      expect(row.trigger).toBe(entry.trigger)
      expect(row.target).toBe(entry.target)
      expect(row.params).toBe(entry.params)
    })
  }

  it('进入 / 返回的目标与参数在源码里有落点', () => {
    const sources = Object.values(SRC_SOURCES).join('\n')
    for (const entry of JUMP_MATRIX) {
      if (entry.status !== 'covered') {
        continue
      }
      const marker = ANCHOR[entry.no]
      expect(sources, `第 ${entry.no} 条缺源码锚点`).toContain(marker)
    }
  })
})

/** 每条目在源码里的落点锚点（改实现即失效，防清单与代码脱节） */
const ANCHOR: Record<number, string> = {
  7: "fromView: 'memory'",
  8: 'consumeSnapshot',
  9: 'requestPreselect',
  10: "name: 'chat'",
  13: 'doBackToList',
}
