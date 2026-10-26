/**
 * tests/debugView.test.ts — 调试视图的页签、取数与五态
 *
 * 覆盖:
 * - 默认进入关键词池：三态计数、待确认别名裁决与分组展开
 * - 说话风格：按人格取数、五维与规则文本、无记录空态
 * - 评估产物：未选目录空态、选择目录后列表与摘要、目录不可读降级
 * - 主请求失败的错误块与重试
 * - 深链直达非默认页签
 */

import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type { EvalFile, KeywordPool, PendingAlias, PersonaSummary, StyleStats } from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { useDebugStore } from '../src/stores/debug'
import DebugView from '../src/views/debug/index.vue'

interface RecordedCall {
  command: string
  args: Record<string, unknown>
}

interface DebugFixture {
  failKeywords: boolean
  failEvalList: boolean
  resolved: Array<{ alias: string; action: string }>
  pool: KeywordPool
  pending: PendingAlias[]
  personas: PersonaSummary[]
  style: StyleStats | null
  files: EvalFile[]
  pickDir: string | null
  /** 非空时关键词请求挂起，直到用例放行（用于断言加载态） */
  keywordsGate: Promise<void> | null
}

function samplePool(): KeywordPool {
  return {
    total: 3,
    canonical_count: 1,
    alias_count: 1,
    pending_count: 1,
    keywords: [
      {
        keyword: '珊瑚菌',
        use_count: 46,
        status: 'canonical',
        canonical_id: null,
        canonical_keyword: null,
        created_at: 1_700_000_000_000,
      },
      {
        keyword: '小 rama',
        use_count: 18,
        status: 'alias',
        canonical_id: 1,
        canonical_keyword: '珊瑚菌',
        created_at: 1_700_000_000_000,
      },
      {
        keyword: '珊瑚',
        use_count: 5,
        status: 'pending',
        canonical_id: 1,
        canonical_keyword: '珊瑚菌',
        created_at: 1_700_000_000_000,
      },
    ],
  }
}

function sampleStyle(): StyleStats {
  return {
    persona_uid: 'rama-0001',
    sample_count: 312,
    baseline_version: 2,
    status: 'ready',
    status_label: '充分',
    rule_source: 'template',
    rule_source_label: '模板',
    rule_text: '多用短句，句尾常带语气词。',
    stats_json: JSON.stringify({
      sample_count: 312,
      total_chars: 5200,
      total_sentences: 260,
      word_freq: [['呢', 40]],
      sentence_len_mean: 18.4,
      sentence_len_p25: 11,
      sentence_len_p75: 26,
      slash_count: 4,
      comma_count: 120,
      newline_count: 30,
      exclaim_count: 8,
      question_count: 22,
      ellipsis_count: 18,
      paren_count: 3,
      tilde_count: 5,
      sentiment_mean: 0.32,
      sentiment_std: 0.4,
      sentiment_n: 310,
      interjection_count: 52,
      sentiment_word_messages: 240,
      topic_freq: [['工作', 9]],
    }),
    updated_at: 1_760_000_000_000,
  }
}

function sampleFiles(): EvalFile[] {
  return [
    { name: 'report-B1.json', path: 'C:/eval/report-B1.json', size: 2048, modified_at: 1_760_000_000_000 },
    { name: 'run-2026.json', path: 'C:/eval/run-2026.json', size: 512, modified_at: 1_750_000_000_000 },
  ]
}

function makeRuntime(overrides: Partial<DebugFixture> = {}) {
  const calls: RecordedCall[] = []
  const fixture: DebugFixture = {
    failKeywords: false,
    failEvalList: false,
    resolved: [],
    pool: samplePool(),
    pending: [
      { alias_id: 7, alias_keyword: '珊瑚', canonical_keyword: '珊瑚菌', created_at: 1_700_000_000_000 },
    ],
    personas: [
      {
        uid: 'rama-0001',
        name: '小珊',
        kind: 'rama',
        source: 'local',
        is_active: true,
        created_at: 1_700_000_000_000,
      },
    ],
    style: sampleStyle(),
    files: sampleFiles(),
    pickDir: 'C:/eval',
    keywordsGate: null,
    ...overrides,
  }

  const runtime: TauriRuntime = {
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        switch (command) {
          case 'list_keywords': {
            if (fixture.failKeywords) {
              throw new Error('数据库读取失败')
            }
            if (fixture.keywordsGate) {
              await fixture.keywordsGate
            }
            return fixture.pool
          }
          case 'list_pending_aliases':
            return { total: fixture.pending.length, aliases: fixture.pending }
          case 'resolve_alias': {
            const alias = String(args.alias)
            fixture.resolved.push({ alias, action: String(args.action) })
            fixture.pending = fixture.pending.filter((item) => item.alias_keyword !== alias)
            return { alias, canonical_keyword: '珊瑚菌', status: 'alias', already_applied: false }
          }
          case 'get_personas':
            return fixture.personas
          case 'get_style_stats':
            return fixture.style
          case 'pick_eval_dir':
            return fixture.pickDir
          case 'list_eval_files': {
            if (fixture.failEvalList) {
              throw new Error('目录不在允许读取范围内，拒绝访问')
            }
            return { dir: String(args.dir), total: fixture.files.length, files: fixture.files }
          }
          case 'read_eval_result':
            return {
              persona_uid: 'rama-0001',
              judge_used: true,
              embedding_used: false,
              generated_at: '2026-09-01T00:00:00Z',
              variants: [{ variant_id: 'B1', fact_score: 0.8, params: { ablation: 'B1' } }],
              auxiliary: { evidence_traceability_rate: 0.9 },
            }
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
    event: {
      listen: async () => () => {},
      emit: async () => {},
    },
  }

  return { runtime, calls, fixture }
}

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: { template: '<div />' } },
      { path: '/debug/:tab?', name: 'debug', component: { template: '<div />' } },
    ],
  })
}

interface MountOptions {
  overrides?: Partial<DebugFixture>
  evalDir?: string
}

async function mountDebug(path = '/debug/keywords', options: MountOptions = {}) {
  const { runtime, calls, fixture } = makeRuntime(options.overrides)
  setTauriRuntime(runtime)
  const pinia = createPinia()
  setActivePinia(pinia)
  if (options.evalDir) {
    useDebugStore().evalDir = options.evalDir
  }
  const router = makeRouter()
  await router.push(path)
  await router.isReady()
  const wrapper = mount(DebugView, { global: { plugins: [pinia, router] } })
  await flushPromises()
  await nextTick()
  return { wrapper, calls, fixture, router }
}

beforeEach(() => {
  window.localStorage.clear()
})

afterEach(() => {
  window.localStorage.clear()
  setTauriRuntime(null)
})

describe('调试视图', () => {
  it('默认进入关键词池，展示三态计数与待确认别名', async () => {
    const { wrapper } = await mountDebug()

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
    const stats = wrapper.findAll('.stat')
    expect(stats).toHaveLength(3)
    expect(stats[0]?.text()).toContain('规范词')
    expect(wrapper.findAll('.alias-row')).toHaveLength(1)
    expect(wrapper.find('.alias-row').text()).toContain('珊瑚')
    expect(wrapper.find('.alias-row').text()).toContain('珊瑚菌')
    // 默认只展开规范词：可见词条只有一条
    expect(wrapper.findAll('.keyword-row')).toHaveLength(1)
    expect(wrapper.find('.keyword-row').text()).toContain('珊瑚菌')
  })

  it('折叠组展开后显示该状态的词条', async () => {
    const { wrapper } = await mountDebug()

    const aliasHead = wrapper
      .findAll('.group-head')
      .find((item) => item.text().includes('已确认别名'))
    expect(aliasHead?.attributes('aria-expanded')).toBe('false')

    await aliasHead?.trigger('click')
    await nextTick()

    expect(aliasHead?.attributes('aria-expanded')).toBe('true')
    const rows = wrapper.findAll('.keyword-row').map((item) => item.text())
    expect(rows.some((text) => text.includes('小 rama'))).toBe(true)
  })

  it('确认合并后调用裁决命令并重新取数', async () => {
    const { wrapper, calls, fixture } = await mountDebug()

    const confirm = wrapper.findAll('button').find((item) => item.text() === '确认合并')
    await confirm?.trigger('click')
    await flushPromises()

    expect(fixture.resolved).toEqual([{ alias: '珊瑚', action: 'confirm' }])
    expect(calls.filter((call) => call.command === 'list_keywords')).toHaveLength(2)
    // 裁决后待确认区消失
    expect(wrapper.findAll('.alias-row')).toHaveLength(0)
  })

  it('请求在途时进入加载态并给出骨架', async () => {
    let release: () => void = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    const { wrapper } = await mountDebug('/debug/keywords', { overrides: { keywordsGate: gate } })

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('loading')
    expect(wrapper.findAll('.skeleton').length).toBeGreaterThan(0)

    release()
    await flushPromises()
    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
  })

  it('切换人格后按该人格重新取统计', async () => {
    const { wrapper, calls } = await mountDebug('/debug/style', {
      overrides: {
        personas: [
          {
            uid: 'char-0002',
            name: '阿岚',
            kind: 'char',
            source: 'qq',
            is_active: false,
            created_at: 1_700_000_000_000,
          },
          {
            uid: 'rama-0001',
            name: '小珊',
            kind: 'rama',
            source: 'local',
            is_active: true,
            created_at: 1_700_000_000_000,
          },
        ],
      },
    })

    // 默认落在 rama 人格
    expect(wrapper.find('.style-persona .select-value').text()).toBe('小珊')

    await wrapper.find('.style-persona .select-trigger').trigger('click')
    const option = wrapper.findAll('.select-option').find((item) => item.text() === '阿岚')
    await option?.trigger('click')
    await flushPromises()

    const styleCalls = calls.filter((call) => call.command === 'get_style_stats')
    expect(styleCalls.at(-1)?.args.personaUid).toBe('char-0002')
  })

  it('说话风格按人格取数并渲染五维与规则文本', async () => {
    const { wrapper, calls } = await mountDebug('/debug/style')

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
    expect(calls.some((call) => call.command === 'get_style_stats')).toBe(true)
    expect(wrapper.find('.rule-text').text()).toContain('多用短句')

    const titles = wrapper.findAll('.block-title').map((item) => item.text())
    expect(titles).toContain('口癖词')
    expect(titles).toContain('句式句长')
    expect(titles).toContain('情感表达')
    expect(wrapper.text()).toContain('23.1‰')
    expect(wrapper.text()).toContain('76.9%')
  })

  it('无统计记录时说话风格为空态', async () => {
    const { wrapper } = await mountDebug('/debug/style', { overrides: { style: null } })

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('empty')
    expect(wrapper.find('.empty-title').text()).toContain('还没有风格统计')
  })

  it('深链直接进入评估产物页签', async () => {
    const { wrapper } = await mountDebug('/debug/eval')

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('empty')
    expect(wrapper.find('.empty-title').text()).toContain('还没有调试产物')
    // 工具栏与空态各有一个「选择目录」入口
    const pickers = wrapper.findAll('button').filter((item) => item.text() === '选择目录')
    expect(pickers.length).toBeGreaterThan(0)
  })

  it('选择目录后列出产物并打开摘要', async () => {
    const { wrapper, calls } = await mountDebug('/debug/eval')

    const picker = wrapper.findAll('button').find((item) => item.text() === '选择目录')
    await picker?.trigger('click')
    await flushPromises()

    expect(calls.some((call) => call.command === 'pick_eval_dir')).toBe(true)
    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
    const rows = wrapper.findAll('.file-row')
    expect(rows).toHaveLength(2)
    expect(rows[0]?.text()).toContain('report-B1.json')

    await rows[0]?.trigger('click')
    await flushPromises()

    expect(wrapper.find('.artifact-title').text()).toBe('report-B1.json')
    expect(wrapper.text()).toContain('消融报告')
    expect(wrapper.text()).toContain('已使用')
    expect(wrapper.find('.score').text()).toContain('事实')
  })

  it('评估目录不可读时显示降级条并清空列表', async () => {
    const { wrapper } = await mountDebug('/debug/eval', {
      evalDir: 'C:/eval',
      overrides: { failEvalList: true },
    })

    expect(wrapper.find('.debug').attributes('data-degraded')).toBe('dir')
    expect(wrapper.find('.degraded-strip').exists()).toBe(true)
    expect(wrapper.find('.degraded-strip').text()).toContain('评估目录不可读')
    expect(wrapper.findAll('.file-row')).toHaveLength(0)
  })

  it('已有数据时刷新失败只加行内提示，不替换内容', async () => {
    const { wrapper, fixture } = await mountDebug()

    fixture.failKeywords = true
    const refresh = wrapper.find('button[aria-label="刷新当前页签"]')
    await refresh.trigger('click')
    await flushPromises()

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
    expect(wrapper.find('.error-strip').text()).toContain('数据库读取失败')
    expect(wrapper.findAll('.keyword-row').length).toBeGreaterThan(0)
  })

  it('关键词池读取失败显示错误块并可重试', async () => {
    const { wrapper, fixture } = await mountDebug('/debug/keywords', {
      overrides: { failKeywords: true },
    })

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('error')
    expect(wrapper.find('.empty-title').text()).toContain('调试数据读取失败')

    fixture.failKeywords = false
    const retry = wrapper.findAll('button').find((item) => item.text() === '重试')
    await retry?.trigger('click')
    await flushPromises()

    expect(wrapper.find('.debug').attributes('data-view-state')).toBe('default')
    expect(wrapper.findAll('.alias-row')).toHaveLength(1)
  })
})
