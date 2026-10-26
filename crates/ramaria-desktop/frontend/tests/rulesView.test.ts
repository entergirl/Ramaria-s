/**
 * tests/rulesView.test.ts — 规则视图挂载与关键交互
 *
 * parity 清单见 `tests/parity/checklist.ts`（内容与操作在本文件断言，五态逐格覆盖）。
 *
 * 覆盖:
 * - 规则卡渲染且无删除、无列表编辑入口；启停成功按回执更新、失败保持原状态
 * - 人格筛选：切人格清空重取，切回旧人格命中缓存并静默刷新
 * - 详情内编辑：保存转手工、候选规则只改禁忌词、已有文本不允许清空
 * - 详情与证据链：加载、空、失败重试与返回列表
 * - 空态、错误态与降级条（LLM 不可用时编辑仍可用）
 */
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type {
  BehaviorRuleView,
  PersonaSummary,
  RuleEvidenceItem,
} from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { useAppStore } from '../src/stores/app'
import RulesView from '../src/views/rules/index.vue'

const NOW = Date.now()

const RAMA: PersonaSummary = {
  uid: 'rama-0001',
  name: '小珊',
  kind: 'rama',
  source: 'manual',
  is_active: true,
  created_at: NOW - 500000,
}

const CHAR: PersonaSummary = {
  uid: 'char-1001',
  name: '林霁',
  kind: 'char',
  source: 'qq',
  is_active: true,
  created_at: NOW - 400000,
}

function rule(partial: Partial<BehaviorRuleView> = {}): BehaviorRuleView {
  return {
    id: 7,
    persona_uid: 'rama-0001',
    situation: {
      keywords: ['夜间', '边界', '工作'],
      centroid: null,
      response_centroid: null,
      valence_mean: 0.12,
      valence_std: 0.08,
      sample_count: 6,
      presentation_dist: [{ presentation: 'subjective', freq: 0.62 }],
      situation_strength_mean: 3.4,
      time_span_days: 12,
      trait_refs: [],
    },
    reaction: '深夜 23:00 之后不要主动发起高强度话题。',
    params: {
      emotional_intensity: 0.42,
      proactiveness: 0.3,
      detail_level: 0.55,
      formality: 0.4,
    },
    avoid: ['说教'],
    evidence: [{ event_id: 1042, weight: 0.9 }],
    confidence: 0.91,
    stability: 0.8,
    source: 'manual',
    enabled: true,
    created_at: NOW - 7200000,
    updated_at: NOW - 3600000,
    ...partial,
  }
}

function evidenceItems(): RuleEvidenceItem[] {
  return [
    {
      event_id: 1042,
      weight: 0.9,
      title: '深夜聊天',
      summary: '深夜聊到工作压力，情绪偏疲惫。',
      paraphrase: '先接住情绪再谈安排',
      keywords: '夜间,压力',
    },
  ]
}

interface RuntimeConfig {
  personas?: PersonaSummary[]
  rules?: BehaviorRuleView[] | ((uid: string | null) => BehaviorRuleView[])
  evidence?: RuleEvidenceItem[] | (() => RuleEvidenceItem[])
  failOnce?: string[]
  failAlways?: string[]
  holdPersonas?: boolean
}

interface RecordedCall {
  command: string
  args: Record<string, unknown>
}

function makeRuntime(config: RuntimeConfig = {}): {
  runtime: TauriRuntime
  calls: RecordedCall[]
  release: () => void
} {
  const calls: RecordedCall[] = []
  const remaining = new Set(config.failOnce ?? [])
  const always = new Set(config.failAlways ?? [])
  let release = (): void => {}

  const gate = config.holdPersonas
    ? new Promise<void>((resolve) => {
        release = resolve
      })
    : Promise.resolve()

  const rulesOf = (uid: string | null): BehaviorRuleView[] =>
    typeof config.rules === 'function' ? config.rules(uid) : (config.rules ?? [rule()])

  const runtime: TauriRuntime = {
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        if (always.has(command) || remaining.has(command)) {
          remaining.delete(command)
          throw new Error('数据库读取失败')
        }
        switch (command) {
          case 'get_personas':
            await gate
            return config.personas ?? [RAMA, CHAR]
          case 'list_rules': {
            const uid = (args.personaUid as string | null) ?? null
            const list = rulesOf(uid)
            return { persona_uid: uid ?? 'rama-0001', total: list.length, rules: list }
          }
          case 'set_rule_enabled':
            return { id: args.ruleId, enabled: args.enabled }
          case 'edit_rule': {
            const id = args.ruleId as number
            const base = rulesOf(null).find((item) => item.id === id)
            if (!base) {
              throw new Error('规则不存在')
            }
            const reaction =
              args.reaction === null ? base.reaction : (args.reaction as string)
            const avoid =
              args.avoid === null
                ? base.avoid
                : String(args.avoid)
                    .split(',')
                    .filter((item) => item.length > 0)
            return { ...base, reaction, avoid, source: 'manual', updated_at: NOW }
          }
          case 'rule_evidence':
            return {
              rule_id: args.ruleId,
              evidence:
                typeof config.evidence === 'function' ? config.evidence() : (config.evidence ?? evidenceItems()),
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

  return { runtime, calls, release: () => release() }
}

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: { template: '<div />' } },
      { path: '/rules', name: 'rules', component: { template: '<div />' } },
    ],
  })
}

async function mountRules(
  config: RuntimeConfig = {},
  seed?: (pinia: Pinia) => void,
): Promise<{ wrapper: VueWrapper; router: Router; calls: RecordedCall[]; release: () => void }> {
  const { runtime, calls, release } = makeRuntime(config)
  setTauriRuntime(runtime)
  const pinia = createPinia()
  setActivePinia(pinia)
  seed?.(pinia)
  const router = makeRouter()
  await router.push('/rules')
  await router.isReady()
  const wrapper = mount(RulesView, {
    global: { plugins: [pinia, router] },
    attachTo: document.body,
  })
  await flushPromises()
  return { wrapper, router, calls, release }
}

function viewState(wrapper: VueWrapper): string | undefined {
  return wrapper.find('[data-view-state]').attributes('data-view-state')
}

function countOf(calls: RecordedCall[], command: string): number {
  return calls.filter((call) => call.command === command).length
}

function lastArgs(calls: RecordedCall[], command: string): Record<string, unknown> | undefined {
  return calls.filter((call) => call.command === command).at(-1)?.args
}

function button(wrapper: VueWrapper, label: string) {
  const hit = wrapper.findAll('button').find((item) => item.text() === label)
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

/** 浮层经 Teleport 挂到 body，按文案取按钮 */
function domButton(label: string): HTMLButtonElement {
  const hit = Array.from(document.body.querySelectorAll('button')).find(
    (item) => (item.textContent ?? '').trim() === label,
  )
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('规则列表', () => {
  it('渲染规则卡、来源与元信息，且没有删除入口', async () => {
    const { wrapper } = await mountRules({
      rules: [
        rule(),
        rule({
          id: 8,
          source: 'auto',
          reaction: null,
          enabled: false,
          confidence: 0.55,
          stability: 0.4,
        }),
      ],
    })

    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('小珊')
    expect(wrapper.text()).toContain('默认人格')
    expect(wrapper.text()).toContain('共 2 条')
    expect(wrapper.text()).toContain('深夜 23:00 之后不要主动发起高强度话题。')
    expect(wrapper.text()).toContain('（候选规则，仅参数注入）')
    expect(wrapper.text()).toContain('手工')
    expect(wrapper.text()).toContain('自动')
    expect(wrapper.text()).toContain('已启用')
    expect(wrapper.text()).toContain('已停用')
    expect(wrapper.text()).toContain('仅参数注入')
    expect(wrapper.text()).toContain('置信度 0.91')
    expect(wrapper.text()).toContain('稳定性 0.80')
    expect(wrapper.text()).toContain('情感强度 0.42')
    expect(wrapper.text()).toContain('证据 1 条')
    expect(wrapper.text()).toContain('夜间')

    const switches = wrapper.findAll('input[role="switch"]')
    expect(switches).toHaveLength(2)
    expect((switches[0]?.element as HTMLInputElement).checked).toBe(true)
    expect((switches[1]?.element as HTMLInputElement).checked).toBe(false)

    const labels = wrapper.findAll('button').map((item) => item.text())
    expect(labels.some((label) => label.includes('删除'))).toBe(false)
    // 列表只保留详情入口，编辑在详情页完成
    expect(labels.filter((label) => label === '详情')).toHaveLength(2)
    expect(labels).not.toContain('编辑')
    wrapper.unmount()
  })

  it('首屏读取中给出骨架与忙碌标记', async () => {
    const { wrapper, release } = await mountRules({ holdPersonas: true })

    expect(viewState(wrapper)).toBe('loading')
    expect(wrapper.find('[data-view-state]').attributes('aria-busy')).toBe('true')
    expect(wrapper.findAll('.skeleton')).toHaveLength(4)

    release()
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('深夜 23:00 之后不要主动发起高强度话题。')
    wrapper.unmount()
  })

  it('切换人格清空重取，切回旧人格命中缓存并静默刷新', async () => {
    const { wrapper, calls } = await mountRules({
      rules: (uid) =>
        uid === 'char-1001'
          ? [rule({ id: 21, persona_uid: 'char-1001', reaction: '林霁的规则文本' })]
          : [rule()],
    })

    await wrapper.find('#rules-persona-trigger').trigger('click')
    await flushPromises()
    expect(wrapper.find('#rules-personas').exists()).toBe(true)
    const rows = wrapper.findAll('.persona-row')
    expect(rows).toHaveLength(2)

    await rows[1]?.trigger('click')
    await flushPromises()
    expect(lastArgs(calls, 'list_rules')).toEqual({ personaUid: 'char-1001' })
    expect(wrapper.text()).toContain('林霁的规则文本')
    expect(wrapper.text()).not.toContain('深夜 23:00 之后不要主动发起高强度话题。')

    await wrapper.find('#rules-persona-trigger').trigger('click')
    await flushPromises()
    await wrapper.findAll('.persona-row')[0]?.trigger('click')
    await flushPromises()

    const listCalls = calls.filter((call) => call.command === 'list_rules')
    expect(listCalls).toHaveLength(3)
    expect(listCalls[2]?.args).toEqual({ personaUid: 'rama-0001' })
    expect(wrapper.text()).toContain('深夜 23:00 之后不要主动发起高强度话题。')
    wrapper.unmount()
  })

  it('单数条数时最后一条跨两列，抽屉打开后回到单列', async () => {
    const { wrapper } = await mountRules({
      rules: [rule(), rule({ id: 8, reaction: '第二条规则文本。' }), rule({ id: 9 })],
    })

    const cards = wrapper.findAll('.rule-card')
    expect(cards).toHaveLength(3)
    expect(cards[2]?.classes()).toContain('is-wide')

    await wrapper.find('#rules-persona-trigger').trigger('click')
    await flushPromises()
    expect(wrapper.find('#rules-personas').exists()).toBe(true)
    expect(wrapper.findAll('.rule-card')[2]?.classes()).not.toContain('is-wide')
    wrapper.unmount()
  })

  it('无规则时给出去对话的引导', async () => {
    const { wrapper, router } = await mountRules({ rules: [] })

    expect(viewState(wrapper)).toBe('empty')
    expect(wrapper.text()).toContain('还没有规则')
    expect(wrapper.text()).toContain('规则随对话与画像推断自动生成。')

    await button(wrapper, '去对话').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.name).toBe('chat')
    wrapper.unmount()
  })

  it('列表失败给出错误态与重试', async () => {
    const { wrapper } = await mountRules({ failOnce: ['list_rules'] })

    expect(viewState(wrapper)).toBe('error')
    expect(wrapper.text()).toContain('规则加载失败')
    expect(wrapper.text()).toContain('数据库读取失败')

    await button(wrapper, '重试').trigger('click')
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('深夜 23:00 之后不要主动发起高强度话题。')
    wrapper.unmount()
  })

  it('对话模型不可用时给降级条，编辑仍可用', async () => {
    const { wrapper } = await mountRules({}, (pinia) => {
      setActivePinia(pinia)
      useAppStore().degradedReason = 'llm_unavailable'
    })

    expect(wrapper.find('.rules').attributes('data-degraded')).toBe('llm')
    expect(wrapper.text()).toContain('规则自动抽取暂停，编辑与启停仍可用。')

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    const input = wrapper.find('#rule-detail-input')
    expect(input.exists()).toBe(true)
    expect((input.element as HTMLTextAreaElement).disabled).toBe(false)
    wrapper.unmount()
  })
})

describe('规则启停', () => {
  it('启停写库后按回执更新卡片', async () => {
    const { wrapper, calls } = await mountRules()

    await wrapper.find('input[role="switch"]').setValue(false)
    await flushPromises()

    expect(lastArgs(calls, 'set_rule_enabled')).toEqual({ ruleId: 7, enabled: false })
    expect(wrapper.text()).toContain('已停用')
    expect(wrapper.find('.rule-card').classes()).toContain('is-off')
    wrapper.unmount()
  })

  it('启停失败保持原状态', async () => {
    const { wrapper } = await mountRules({ failAlways: ['set_rule_enabled'] })

    await wrapper.find('input[role="switch"]').setValue(false)
    await flushPromises()

    expect(wrapper.text()).toContain('已启用')
    expect((wrapper.find('input[role="switch"]').element as HTMLInputElement).checked).toBe(true)
    wrapper.unmount()
  })
})

describe('详情内编辑', () => {
  it('保存后转为手工并就地更新', async () => {
    const { wrapper, calls } = await mountRules({ rules: [rule({ source: 'auto' })] })

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('规则 #7')

    const textarea = wrapper.find('#rule-detail-input')
    expect((textarea.element as HTMLTextAreaElement).value).toBe(
      '深夜 23:00 之后不要主动发起高强度话题。',
    )
    await textarea.setValue('新的规则文本')
    await flushPromises()

    const save = button(wrapper, '保存')
    expect(save.attributes('disabled')).toBeUndefined()
    await save.trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'edit_rule')).toEqual({
      ruleId: 7,
      reaction: '新的规则文本',
      avoid: null,
    })
    expect((wrapper.find('#rule-detail-input').element as HTMLTextAreaElement).value).toBe(
      '新的规则文本',
    )
    expect(wrapper.text()).toContain('手工')
    expect(wrapper.text()).not.toContain('未保存')
    wrapper.unmount()
  })

  it('已有文本的规则不允许清空，保存保持不可用', async () => {
    const { wrapper } = await mountRules()

    await button(wrapper, '详情').trigger('click')
    await flushPromises()

    await wrapper.find('#rule-detail-input').setValue('   ')
    await flushPromises()

    expect(wrapper.text()).toContain('规则文本不能为空')
    expect(button(wrapper, '保存').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('有未保存改动时返回先确认', async () => {
    const { wrapper } = await mountRules()

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    await wrapper.find('#rule-detail-input').setValue('改了一半')
    await flushPromises()
    expect(wrapper.text()).toContain('未保存')

    await button(wrapper, '规则').trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('放弃未保存的修改？')

    domButton('继续编辑').click()
    await flushPromises()
    expect(wrapper.text()).toContain('规则 #7')

    await button(wrapper, '规则').trigger('click')
    await flushPromises()
    domButton('放弃并返回').click()
    await flushPromises()
    expect(wrapper.text()).toContain('共 1 条')
    wrapper.unmount()
  })

  it('候选规则只改禁忌词时保存不带规则文本', async () => {
    const { wrapper, calls } = await mountRules({
      rules: [rule({ id: 11, source: 'auto', reaction: null })],
    })

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('这是候选规则')

    const input = wrapper.find('.detail-card input[type="text"]')
    await input.setValue('说教，训诫')
    await flushPromises()

    await button(wrapper, '保存').trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'edit_rule')).toEqual({
      ruleId: 11,
      reaction: null,
      avoid: '说教,训诫',
    })
    wrapper.unmount()
  })
})

describe('规则详情与证据', () => {
  it('详情页展示规则内容、参数与证据链，并可返回列表', async () => {
    const { wrapper, calls } = await mountRules()

    await button(wrapper, '详情').trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'rule_evidence')).toEqual({ ruleId: 7 })
    expect(wrapper.text()).toContain('规则 #7')
    expect(wrapper.text()).toContain('规则内容')
    expect(wrapper.text()).toContain('禁忌词')
    const avoidInput = wrapper.find('.detail-card input[type="text"]')
    expect((avoidInput.element as HTMLInputElement).value).toBe('说教')
    expect((wrapper.find('#rule-detail-input').element as HTMLTextAreaElement).value).toBe(
      '深夜 23:00 之后不要主动发起高强度话题。',
    )
    expect(wrapper.text()).toContain('参数与统计')
    expect(wrapper.text()).toContain('主动程度')
    expect(wrapper.text()).toContain('样本数')
    expect(wrapper.text()).toContain('证据链')
    expect(wrapper.text()).toContain('事件 #1042')
    expect(wrapper.text()).toContain('权重 0.90')
    expect(wrapper.text()).toContain('深夜聊天')
    expect(wrapper.text()).toContain('态度（脱敏）：先接住情绪再谈安排')

    await button(wrapper, '规则').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('共 1 条')
    wrapper.unmount()
  })

  it('证据为空时说明原因', async () => {
    const { wrapper } = await mountRules({ evidence: [] })

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('这条规则还没有关联事件证据')
    wrapper.unmount()
  })

  it('证据读取失败可重试', async () => {
    const { wrapper, calls } = await mountRules({ failOnce: ['rule_evidence'] })

    await button(wrapper, '详情').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('数据库读取失败')

    await button(wrapper, '重试').trigger('click')
    await flushPromises()
    expect(countOf(calls, 'rule_evidence')).toBe(2)
    expect(wrapper.text()).toContain('事件 #1042')
    wrapper.unmount()
  })
})
