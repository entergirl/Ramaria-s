/**
 * tests/parity/jumps.test.ts — 跨视图跳转矩阵守卫
 *
 * 覆盖:
 * - 清单结构：编号唯一、触发与目标非空、全局 / 页内范围计数与基线口径一致
 * - 覆盖引用真实存在：条目指到的测试文件里的用例名没有被改名或删除
 * - 与跳转矩阵基线一致：同编号行的触发 / 目标 / 参数逐字一致
 * - 参数 grep 兜底：跳转参数与一次性状态的含义在源码里有对应落点
 *
 * 说明:
 * - 基线表在本文内冻结，不读取仓库外文档；矩阵变更时同批改基线与条目
 * - 测试源码经 `import.meta.glob` 的 `?raw` 导入取得，避免引入 Node 类型依赖
 */

import { describe, expect, it } from 'vitest'

import { JUMP_MATRIX } from './jumps'

const TEST_SOURCES = import.meta.glob('/tests/**/*.test.ts', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>

/** 跳转矩阵基线：与 `jumps.ts` 条目逐字对照，矩阵变更时同批更新 */
const SPEC_MATRIX: MatrixRow[] = [
  { no: '1', trigger: 'needs_setup', target: 'setup', params: '欢迎文案' },
  {
    no: '2',
    trigger: 'downloading_model',
    target: '不切视图',
    params: '全局下载横幅（不含 indexing）',
  },
  { no: '3', trigger: 'ready', target: '上次导航视图（默认 chat）', params: '恢复所在页' },
  {
    no: '4',
    trigger: 'degraded',
    target: '上次导航视图',
    params: '视图内降级条按 degradedReason 分流',
  },
  {
    no: '5',
    trigger: 'fatal_error / 后端连接失败',
    target: 'error',
    params: '错误标题 + 详情',
  },
  { no: '6', trigger: '导航项点击', target: '对应视图', params: '无参数；全屏中忽略' },
  {
    no: '7',
    trigger: '记忆 L1 卡「查看对话」',
    target: 'chat',
    params: "{sessionId, personaUid, fromView:'memory'}",
  },
  {
    no: '8',
    trigger: '对话「返回记忆」',
    target: 'memory',
    params: '恢复人格 / 页签 / 滚动位置',
  },
  {
    no: '9',
    trigger: '导入完成「前往记忆页面」',
    target: 'memory',
    params: '一次性 preselectPersonaUid',
  },
  {
    no: '10',
    trigger: '导入完成「查看导入消息」',
    target: 'chat',
    params: '一次性导入会话模式',
  },
  {
    no: '11',
    trigger: '主动消息通知点击',
    target: 'chat',
    params: "{sessionId, fromView:'proactive'}；已在 chat 则直接打开会话",
  },
  { no: '12', trigger: '首次配置完成', target: '状态机路由', params: '写 appState=ready' },
  { no: '13', trigger: '人格详情操作', target: '页内二级', params: '不跨视图' },
  {
    no: '14',
    trigger: '品牌键',
    target: '关于浮层',
    params: '不切视图；版本 / 检查更新 / 调试开关',
  },
  {
    no: '15',
    trigger: '设置一 / 二级菜单',
    target: '页内子页面',
    params: '默认「基础设置 → 模型与检索」',
  },
]

const SRC_SOURCES = import.meta.glob('/src/{app,components,views}/**/*.{ts,vue}', {
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

function rowOf(no: number): MatrixRow {
  const hit = SPEC_MATRIX.find((row) => row.no === String(no))
  if (!hit) {
    throw new Error(`跳转矩阵基线缺少第 ${no} 条`)
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
        expect(entry.coverage?.length ?? 0, `第 ${entry.no} 条缺覆盖落点`).toBeGreaterThan(0)
        for (const item of entry.coverage ?? []) {
          expect(item.file, `第 ${entry.no} 条缺覆盖文件`).toBeTruthy()
          expect(item.cases.length, `第 ${entry.no} 条缺覆盖用例`).toBeGreaterThan(0)
        }
        expect(entry.naReason).toBeUndefined()
      } else {
        expect(entry.naReason, `第 ${entry.no} 条缺不适用依据`).toBeTruthy()
        expect(entry.coverage).toBeUndefined()
      }
    }
  })

  it('全局 13 条、页内 2 条，与基线口径一致', () => {
    expect(JUMP_MATRIX.filter((entry) => entry.scope === 'global')).toHaveLength(13)
    expect(JUMP_MATRIX.filter((entry) => entry.scope === 'page')).toHaveLength(2)
    expect(JUMP_MATRIX.map((entry) => entry.no)).toEqual(
      Array.from({ length: 15 }, (_, index) => index + 1),
    )
  })

  it('覆盖引用指向真实存在的用例', () => {
    const names = new Map<string, string[]>()
    for (const entry of JUMP_MATRIX) {
      for (const coverage of entry.coverage ?? []) {
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

describe('跳转矩阵与基线一致', () => {
  for (const entry of JUMP_MATRIX) {
    it(`第 ${entry.no} 条与基线同编号行一致`, () => {
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
      for (const marker of ANCHOR[entry.no] ?? []) {
        expect(sources, `第 ${entry.no} 条缺源码锚点`).toContain(marker)
      }
    }
  })
})

/** 每条目在源码里的落点锚点（改实现即失效，防清单与代码脱节） */
const ANCHOR: Record<number, string[]> = {
  1: ["app.appState === 'needs_setup'"],
  2: ['DownloadBanner'],
  3: ["state === 'ready' || state === 'degraded'"],
  4: ['degradedReason'],
  5: ["app.appState === 'fatal_error'"],
  6: ['router.push({ name: id })'],
  7: ["fromView: 'memory'"],
  8: ["fromView === 'memory'", 'consumeSnapshot'],
  9: ['requestPreselect'],
  10: ["name: 'chat'"],
  11: ["fromView: 'proactive'"],
  12: ['FULLSCREEN_ROUTES.has(current)'],
  13: ['doBackToList'],
  14: ['aboutOpen'],
  15: ['defaultSectionId'],
}
