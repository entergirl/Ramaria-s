/**
 * tests/memoryView.test.ts — 记忆视图挂载与关键交互
 *
 * 覆盖:
 * - L1 / L2 首页渲染、页签计数与视图主态
 * - 页签切换复用已装载数据（不重复请求）
 * - 人格筛选切换重新取数，深度处理入口只对导入人格出现
 * - 空态、错误态与重试
 * - L1 来源会话摘录、撤回占位与回对话跳转
 * - L3 三层画像、证据链展开与重载画像
 * - 知识分组顺序与更新历史
 */
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type {
  FactList,
  MemoryEvent,
  MemoryL1,
  PersonaSummary,
  PersonalityProfile,
  ProfileStatus,
  SessionDetail,
  TraitDetail,
  TraitEvidenceChain,
} from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { useAppStore } from '../src/stores/app'
import MemoryView from '../src/views/memory/index.vue'

const NOW = Date.now()

const RAMA: PersonaSummary = {
  uid: 'rama-0001',
  name: '小珊',
  kind: 'rama',
  source: 'manual',
  is_active: true,
  created_at: NOW - 500000,
}

const SELF: PersonaSummary = {
  uid: 'user-0001',
  name: '我',
  kind: 'user',
  source: 'system',
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

function l1(partial: Partial<MemoryL1> = {}): MemoryL1 {
  return {
    id: 'l1-1',
    session_id: 's1',
    summary: '聊了周末独自探店的事。',
    keywords: '咖啡馆,独处,探店',
    atmosphere: '平静',
    valence: 0.42,
    salience: 0.61,
    persona_uid: 'rama-0001',
    created_at: NOW - 7200000,
    occurred_at: NOW - 7200000,
    time_period: '下午',
    context_json: JSON.stringify({
      chat_partners: ['user-0001', 'rama-0001'],
      message_count: 8,
    }),
    ...partial,
  }
}

function l2(partial: Partial<MemoryEvent> = {}): MemoryEvent {
  return {
    id: 1042,
    persona_uid: 'rama-0001',
    title: '独自探店',
    summary: '对独自探店持犹豫但开放的态度。',
    keywords: '探店,委托提醒',
    valence: 0.38,
    confidence: 0.86,
    presentation: 'subjective',
    share: 0.75,
    attitude: '回避中试探',
    salience: 0.55,
    created_at: NOW - 7000000,
    start: NOW - 7200000,
    end: NOW - 6900000,
    ...partial,
  }
}

function trait(partial: Partial<TraitDetail> = {}): TraitDetail {
  return {
    id: 1,
    label: '谨慎性',
    meaning: '面对陌生情境时倾向先观察、小步试探。',
    confidence: 0.68,
    evidence: 12,
    consistency: 0.71,
    layer: 'base',
    not_meaning: '不是胆小',
    trigger: '陌生社交',
    suppress: '被明确鼓励',
    related: null,
    seq: 1,
    source: 'inference',
    status: 'active',
    created_at: NOW - 86400000,
    ...partial,
  }
}

function profile(base = [trait()], primary: TraitDetail[] = [], accent: TraitDetail[] = []): PersonalityProfile {
  return { persona_uid: 'rama-0001', base, primary, accent }
}

function status(partial: Partial<ProfileStatus> = {}): ProfileStatus {
  return {
    persona_uid: 'rama-0001',
    n_total_eff: 42,
    active_trait_count: 2,
    status: 'preliminary',
    status_text: '画像初步，随对话持续收敛',
    ...partial,
  }
}

function facts(): FactList {
  return {
    persona_uid: 'rama-0001',
    grouped: {
      兴趣爱好: [
        {
          id: 1,
          field: '兴趣爱好',
          content: '用户养成了睡前阅读的习惯。',
          status: 'active',
          tier: 'stable',
          confidence: 0.74,
          source: 'event',
          keyword_hint: '阅读,睡前仪式',
          version_of: null,
          created_at: NOW - 3600000,
        },
      ],
      基础信息: [
        {
          id: 2,
          field: '基础信息',
          content: '用户住在城市北侧。',
          status: 'active',
          tier: 'stable',
          confidence: 0.9,
          source: 'manual',
          keyword_hint: null,
          version_of: null,
          created_at: NOW - 7200000,
        },
      ],
    },
    versions: {
      '1': [
        {
          id: 9,
          field: '兴趣爱好',
          content: '用户偶尔阅读。',
          status: 'superseded',
          tier: 'stable',
          confidence: 0.5,
          source: 'event',
          keyword_hint: null,
          version_of: null,
          created_at: NOW - 86400000,
        },
        {
          id: 1,
          field: '兴趣爱好',
          content: '用户养成了睡前阅读的习惯。',
          status: 'active',
          tier: 'stable',
          confidence: 0.74,
          source: 'event',
          keyword_hint: '阅读,睡前仪式',
          version_of: 9,
          created_at: NOW - 3600000,
        },
      ],
    },
  }
}

function evidence(): TraitEvidenceChain[] {
  return [
    {
      trait_id: 1,
      trait_label: '谨慎性',
      total_evidence: 3,
      support_count: 2,
      contradict_count: 1,
      neutral_count: 0,
      evidence_events: [
        {
          event_id: 1042,
          title: '独自探店',
          summary: '对陌生社交犹豫但开放。',
          confidence: 0.86,
          valence: 0.38,
          salience: 0.55,
          attitude: '回避中试探',
          paraphrase: '面对陌生环境先观察',
          motives: null,
          l1_sources: [
            {
              l1_id: 'l1-1',
              summary: '聊了周末独自探店的事。',
              evidence_notes: ['提到想进去坐坐'],
              atmosphere: '平静',
              valence: 0.42,
              weight: 0.72,
            },
          ],
        },
      ],
    },
  ]
}

function session(): SessionDetail {
  return {
    id: 's1',
    started_at: NOW - 7300000,
    ended_at: NOW - 7000000,
    persona_uid: 'rama-0001',
    channel: 'local',
    total_messages: 8,
    has_more: false,
    messages: [
      {
        id: 'm1',
        role: 'user',
        content: '今天路过一家新开的咖啡馆。',
        persona_uid: 'user-0001',
        created_at: NOW - 7200000,
        is_proactive: false,
        sender_ref: null,
        sender_name: null,
        recalled_at: null,
      },
      {
        id: 'm2',
        role: 'assistant',
        content: '那就当成一次小小的探险吧。',
        persona_uid: 'rama-0001',
        created_at: NOW - 7150000,
        is_proactive: false,
        sender_ref: null,
        sender_name: null,
        recalled_at: null,
      },
      {
        id: 'm3',
        role: 'user',
        content: '这句已经撤回。',
        persona_uid: 'user-0001',
        created_at: NOW - 7100000,
        is_proactive: false,
        sender_ref: null,
        sender_name: null,
        recalled_at: NOW - 7090000,
      },
    ],
  }
}

interface RuntimeConfig {
  personas?: PersonaSummary[]
  l1?: MemoryL1[] | ((uid: string | null) => MemoryL1[])
  l2?: MemoryEvent[]
  profile?: PersonalityProfile | null
  status?: ProfileStatus | null
  facts?: FactList | null
  session?: SessionDetail | null
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
            return config.personas ?? [RAMA, SELF]
          case 'get_l1_memories': {
            const uid = (args.personaUid as string | null) ?? null
            if (typeof config.l1 === 'function') {
              return config.l1(uid)
            }
            return config.l1 ?? [l1()]
          }
          case 'get_l2_events':
            return config.l2 ?? [l2()]
          case 'get_personality_profile':
            return config.profile === undefined ? profile([trait()], [trait({ id: 2, label: '自我照顾倾向', layer: 'primary' })]) : config.profile
          case 'get_profile_status':
            return config.status === undefined ? status() : config.status
          case 'get_facts':
            return config.facts === undefined ? facts() : config.facts
          case 'get_session':
            return config.session ?? session()
          case 'get_trait_evidence':
            return evidence()
          case 'refresh_persona':
            return 'ok'
          case 'regenerate_import_pipeline':
            return {
              l1_regenerated: 2,
              l1_failed: 0,
              total_sessions: 2,
              early_terminated: false,
              remaining_skipped: 0,
              message: '已完成',
            }
          case 'trigger_memory_pipeline':
            return 'ok'
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
      { path: '/memory', name: 'memory', component: { template: '<div />' } },
      { path: '/import', name: 'import', component: { template: '<div />' } },
      { path: '/settings', name: 'settings', component: { template: '<div />' } },
    ],
  })
}

async function mountMemory(config: RuntimeConfig = {}): Promise<{
  wrapper: VueWrapper
  router: Router
  calls: RecordedCall[]
  release: () => void
}> {
  const { runtime, calls, release } = makeRuntime(config)
  setTauriRuntime(runtime)
  const router = makeRouter()
  await router.push('/memory')
  await router.isReady()
  const pinia = createPinia()
  const wrapper = mount(MemoryView, {
    global: { plugins: [pinia, router] },
    attachTo: document.body,
  })
  setActivePinia(pinia)
  await flushPromises()
  return { wrapper, router, calls, release }
}

function countOf(calls: RecordedCall[], command: string): number {
  return calls.filter((call) => call.command === command).length
}

function segment(wrapper: VueWrapper, label: string) {
  const hit = wrapper.findAll('.segmented-item').find((item) => item.text().includes(label))
  if (!hit) {
    throw new Error(`未找到页签: ${label}`)
  }
  return hit
}

function button(wrapper: VueWrapper, label: string) {
  const hit = wrapper.findAll('button').find((item) => item.text() === label)
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('记忆视图', () => {
  it('首页渲染 L1 卡片、页签计数与视图主态', async () => {
    const { wrapper } = await mountMemory()

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    expect(wrapper.text()).toContain('小珊')
    expect(wrapper.text()).toContain('默认人格')
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')
    expect(wrapper.text()).toContain('咖啡馆')
    expect(wrapper.text()).toContain('+0.42')
    expect(wrapper.text()).toContain('0.61')
    expect(wrapper.text()).toContain('2 人参与')
    // 页签计数：L1 1 / L2 1 / L3 2 / 知识 2
    expect(wrapper.findAll('.segmented-count').map((node) => node.text())).toEqual([
      '1',
      '1',
      '2',
      '2',
    ])
    // 页签为无背板的独立键（plain 档）
    expect(wrapper.find('.segmented').classes()).toContain('segmented--plain')
    expect(wrapper.find('#memory-persona-trigger').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('深度处理导入的消息')
    wrapper.unmount()
  })

  it('首屏读取中给出骨架与忙碌标记', async () => {
    const { wrapper, release } = await mountMemory({ holdPersonas: true })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('loading')
    expect(wrapper.find('[data-view-state]').attributes('aria-busy')).toBe('true')
    expect(wrapper.findAll('.skeleton')).toHaveLength(3)

    release()
    await flushPromises()
    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')
    wrapper.unmount()
  })

  it('索引未就绪时给降级条，浏览不受影响且可跳设置', async () => {
    const { wrapper, router } = await mountMemory()
    const app = useAppStore()
    app.indexStatus = {
      status: 'building',
      loaded: 12,
      total: 40,
      failed: 0,
      percent: 30,
      updated_at: NOW,
    }
    await flushPromises()

    expect(wrapper.find('[data-view-state]').attributes('data-degraded')).toBe('index')
    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    expect(wrapper.text()).toContain('索引尚未就绪')
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')

    await button(wrapper, '去设置').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.name).toBe('settings')
    wrapper.unmount()
  })

  it('L1 时间以发生时间为准，悬停补入库时间', async () => {
    const occurredAt = new Date(2026, 9, 12, 14, 0).getTime()
    const { wrapper } = await mountMemory({
      l1: [l1({ occurred_at: occurredAt, created_at: NOW })],
    })

    expect(wrapper.text()).toContain('10月12日')
    const hover = wrapper.findAll('[title]').find((node) => node.attributes('title')?.includes('入库'))
    expect(hover?.attributes('title')).toContain('入库：')
    wrapper.unmount()
  })

  it('切换页签复用已装载数据，不再请求', async () => {
    const { wrapper, calls } = await mountMemory()

    await segment(wrapper, 'L2 事件').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('独自探店')
    expect(wrapper.text()).toContain('置信度 86%')
    expect(wrapper.text()).toContain('共享度 · 高')

    await segment(wrapper, 'L1 摘要').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')
    expect(countOf(calls, 'get_l1_memories')).toBe(1)
    expect(countOf(calls, 'get_l2_events')).toBe(1)
    expect(countOf(calls, 'get_personality_profile')).toBe(1)
    expect(countOf(calls, 'get_facts')).toBe(1)
    wrapper.unmount()
  })

  it('空数据给引导空态并可跳转导入', async () => {
    const { wrapper, router } = await mountMemory({
      l1: [],
      l2: [],
      profile: profile([], [], []),
      status: status({ n_total_eff: 0, active_trait_count: 0, status: 'insufficient' }),
      facts: { persona_uid: 'rama-0001', grouped: {}, versions: {} },
    })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('empty')
    expect(wrapper.text()).toContain('这里还没有记忆')
    await button(wrapper, '去导入').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.name).toBe('import')
    wrapper.unmount()
  })

  it('主请求全部失败进错误态，重试后恢复', async () => {
    const { wrapper } = await mountMemory({
      failOnce: [
        'get_l1_memories',
        'get_l2_events',
        'get_personality_profile',
        'get_profile_status',
        'get_facts',
      ],
    })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('error')
    expect(wrapper.text()).toContain('记忆加载失败')
    expect(wrapper.text()).toContain('数据库读取失败')

    await button(wrapper, '重试').trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')
    wrapper.unmount()
  })

  it('L1 详情按需取来源摘录，撤回消息只留占位，并可回对话', async () => {
    const { wrapper, router, calls } = await mountMemory()

    await button(wrapper, '详情').trigger('click')
    await flushPromises()

    const excerptCall = calls.find((call) => call.command === 'get_session')
    expect(excerptCall?.args).toEqual({ sessionId: 's1', limit: 3, offset: null })
    expect(wrapper.text()).toContain('今天路过一家新开的咖啡馆。')
    expect(wrapper.text()).toContain('消息已撤回')
    expect(wrapper.text()).not.toContain('这句已经撤回。')
    expect(wrapper.text()).toContain('8 条 → 1 条摘要')
    expect(wrapper.text()).toContain('你、小珊')

    await button(wrapper, '查看对话').trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.name).toBe('chat')
    expect(router.currentRoute.value.query.sessionId).toBe('s1')
    wrapper.unmount()
  })

  it('L3 渲染三层画像，展开证据链并重载画像', async () => {
    const { wrapper, calls } = await mountMemory()

    await segment(wrapper, 'L3 性格').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('底色层')
    expect(wrapper.text()).toContain('主色调层')
    expect(wrapper.text()).toContain('点缀层')
    expect(wrapper.text()).toContain('画像状态 · 初步')
    expect(wrapper.text()).toContain('谨慎性')
    expect(wrapper.text()).toContain('自我照顾倾向')

    await button(wrapper, '展开证据').trigger('click')
    await flushPromises()
    const evidenceCall = calls.find((call) => call.command === 'get_trait_evidence')
    expect(evidenceCall?.args).toEqual({ personaUid: 'rama-0001', traitId: 1 })
    expect(wrapper.text()).toContain('支持 2')
    expect(wrapper.text()).toContain('矛盾 1')
    expect(wrapper.text()).toContain('聊了周末独自探店的事。')
    expect(wrapper.text()).toContain('权重 0.72')

    await button(wrapper, '重载画像').trigger('click')
    await flushPromises()
    expect(calls.some((call) => call.command === 'refresh_persona')).toBe(true)
    wrapper.unmount()
  })

  it('知识页按固定字段顺序分组，更新历史标出当前版本', async () => {
    const { wrapper } = await mountMemory()

    await segment(wrapper, '知识').trigger('click')
    await flushPromises()
    const text = wrapper.text()
    expect(text.indexOf('基础信息')).toBeGreaterThanOrEqual(0)
    expect(text.indexOf('基础信息')).toBeLessThan(text.indexOf('兴趣爱好'))
    expect(text).toContain('用户养成了睡前阅读的习惯。')

    await button(wrapper, '更新历史').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('用户偶尔阅读。')
    expect(wrapper.text()).toContain('当前')
    wrapper.unmount()
  })

  it('切换人格重新取数，深度处理入口只对导入人格出现', async () => {
    const { wrapper, calls } = await mountMemory({
      personas: [RAMA, SELF, CHAR],
      l1: (uid) =>
        uid === 'char-1001'
          ? [l1({ id: 'l1-9', persona_uid: 'char-1001', summary: '林霁聊了拍照的事。' })]
          : [l1()],
    })

    expect(wrapper.text()).not.toContain('深度处理导入的消息')
    await wrapper.find('#memory-persona-trigger').trigger('click')
    await flushPromises()
    expect(wrapper.find('#memory-personas').exists()).toBe(true)
    const rows = wrapper.findAll('.persona-row')
    expect(rows).toHaveLength(3)
    await rows[2]?.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('林霁')
    expect(wrapper.text()).toContain('历史导入')
    expect(wrapper.text()).toContain('林霁聊了拍照的事。')
    expect(wrapper.text()).toContain('深度处理导入的消息')
    expect(countOf(calls, 'get_l1_memories')).toBe(2)

    await button(wrapper, '深度处理导入的消息').trigger('click')
    await flushPromises()
    const deep = calls.find((call) => call.command === 'regenerate_import_pipeline')
    expect(deep?.args).toEqual({ personaUid: 'char-1001' })
    wrapper.unmount()
  })
})
