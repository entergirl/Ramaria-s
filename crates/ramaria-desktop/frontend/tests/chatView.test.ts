/**
 * tests/chatView.test.ts — 对话视图挂载与关键交互
 *
 * 覆盖:
 * - 启动自动打开最近的进行中本地会话，并渲染人格名与消息
 * - 助手回复按 `||` 拆成多条气泡，撤回消息折叠为居中系统条
 * - 会话卡片可打开并切换到社交通道会话，只读会话隐藏输入区
 */
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type { SessionDetail, SessionSummary } from '../src/core/api/dto'
import { setTauriRuntime } from '../src/core/transport'
import ChatView from '../src/views/chat/index.vue'
import { installChatRuntime, type ChatRuntimeOptions } from './helpers/chatRuntime'

const NOW = Date.now()

const RAMA = {
  uid: 'rama-0001',
  name: 'Rama',
  kind: 'rama',
  source: 'manual',
  ref_id: null,
  avatar: null,
  config: null,
  description: null,
  is_active: true,
  created_at: NOW - 100000,
  updated_at: NOW - 100000,
}

const SELF = {
  uid: 'user-0001',
  name: '我',
  kind: 'user',
  source: 'system',
  ref_id: null,
  avatar: null,
  config: null,
  description: null,
  is_active: true,
  created_at: NOW - 100000,
  updated_at: NOW - 100000,
}

function summary(patch: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: 's1',
    started_at: NOW - 60000,
    ended_at: null,
    message_count: 3,
    unread: 0,
    persona_uid: 'rama-0001',
    channel: 'local',
    external_ref: null,
    ...patch,
  }
}

function detail(channel: string, messages: SessionDetail['messages']): SessionDetail {
  return {
    id: 's1',
    started_at: NOW - 60000,
    ended_at: null,
    persona_uid: 'rama-0001',
    channel,
    total_messages: messages.length,
    has_more: false,
    messages,
  }
}

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: { template: '<div />' } },
      { path: '/memory', name: 'memory', component: { template: '<div />' } },
      { path: '/settings', name: 'settings', component: { template: '<div />' } },
    ],
  })
}

function installRuntime(payload: {
  sessions: SessionSummary[]
  detail: SessionDetail
}): void {
  setTauriRuntime({
    core: {
      invoke: async (command: string) => {
        switch (command) {
          case 'list_personas_full':
            return [RAMA, SELF]
          case 'list_sessions':
            return payload.sessions
          case 'get_session':
            return payload.detail
          case 'check_privacy':
            return { status: 'NotNeeded' }
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
    event: {
      listen: async () => () => {},
      emit: async () => {},
    },
  })
}

async function mountChat(): Promise<VueWrapper> {
  const router = makeRouter()
  await router.push('/')
  await router.isReady()
  const wrapper = mount(ChatView, {
    global: { plugins: [createPinia(), router] },
    attachTo: document.body,
  })
  await flushPromises()
  return wrapper
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('对话视图', () => {
  it('自动打开进行中的本地会话并渲染人格与消息', async () => {
    installRuntime({
      sessions: [summary()],
      detail: detail('local', [
        {
          id: 'm1',
          role: 'user',
          content: '今天有点累',
          persona_uid: 'rama-0001',
          created_at: NOW - 40000,
          is_proactive: false,
          sender_ref: null,
          sender_name: null,
          recalled_at: null,
        },
        {
          id: 'm2',
          role: 'assistant',
          content: '先歇一会儿||别硬撑',
          persona_uid: 'rama-0001',
          created_at: NOW - 30000,
          is_proactive: false,
          sender_ref: null,
          sender_name: null,
          recalled_at: null,
        },
        {
          id: 'm3',
          role: 'user',
          content: '算了',
          persona_uid: 'rama-0001',
          created_at: NOW - 20000,
          is_proactive: false,
          sender_ref: null,
          sender_name: null,
          recalled_at: NOW - 19000,
        },
      ]),
    })

    const wrapper = await mountChat()
    expect(wrapper.text()).toContain('Rama')
    expect(wrapper.text()).toContain('今天有点累')
    expect(wrapper.text()).toContain('先歇一会儿')
    expect(wrapper.text()).toContain('别硬撑')
    // 两条助手气泡 + 一条用户气泡；撤回消息不保留气泡
    expect(wrapper.findAll('.bubble')).toHaveLength(3)
    // 本地会话助手用左侧头像，用户用右侧「我」人格头像
    expect(wrapper.findAll('.chat-avatar').map((node) => node.text())).toEqual(['我', 'R'])
    expect(wrapper.find('.msg-recalled').text()).toContain('你撤回了一条消息')
    expect(wrapper.find('textarea').exists()).toBe(true)
    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    wrapper.unmount()
  })

  it('从会话卡片切换到社交通道会话后隐藏输入区', async () => {
    installRuntime({
      sessions: [
        summary({ id: 's1', channel: 'local', ended_at: NOW - 10000 }),
        summary({
          id: 's2',
          channel: 'qq',
          persona_uid: 'user-1001',
          started_at: NOW - 120000,
          message_count: 1,
        }),
      ],
      detail: detail('qq', [
        {
          id: 'q1',
          role: 'user',
          content: '@你 晚上一起看电影吗',
          persona_uid: 'user-1001',
          created_at: NOW - 100000,
          is_proactive: false,
          sender_ref: '10001',
          sender_name: '小珊',
          recalled_at: null,
        },
      ]),
    })

    const wrapper = await mountChat()
    await wrapper.find('.avatar').trigger('click')
    await flushPromises()
    const rows = wrapper.findAll('.session-row')
    expect(rows.length).toBe(2)
    await rows[1]?.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('只读')
    expect(wrapper.text()).toContain('小珊')
    expect(wrapper.find('textarea').exists()).toBe(false)
    expect(wrapper.find('.readonly-bar').exists()).toBe(true)
    expect(wrapper.find('[data-restricted]').attributes('data-restricted')).toBe('readonly')
    wrapper.unmount()
  })
})

describe('跳转来源', () => {
  async function mountFrom(query: Record<string, string>): Promise<{
    wrapper: VueWrapper
    router: Router
  }> {
    const router = makeRouter()
    await router.push({ name: 'chat', query })
    await router.isReady()
    const wrapper = mount(ChatView, {
      global: { plugins: [createPinia(), router] },
      attachTo: document.body,
    })
    await flushPromises()
    return { wrapper, router }
  }

  it('从记忆进入时给出返回入口，查询被一次性消费', async () => {
    installRuntime({
      sessions: [summary()],
      detail: detail('local', [
        {
          id: 'm1',
          role: 'user',
          content: '今天有点累',
          persona_uid: 'rama-0001',
          created_at: NOW - 40000,
          is_proactive: false,
          sender_ref: null,
          sender_name: null,
          recalled_at: null,
        },
      ]),
    })

    const { wrapper, router } = await mountFrom({ sessionId: 's1', fromView: 'memory' })
    expect(router.currentRoute.value.query).toEqual({})

    const entry = wrapper.find('.head-actions .btn--ghost')
    expect(entry.exists()).toBe(true)
    expect(entry.text()).toContain('返回记忆')

    await entry.trigger('click')
    await flushPromises()
    expect(router.currentRoute.value.name).toBe('memory')
    wrapper.unmount()
  })

  it('直接进入对话不出现返回入口', async () => {
    installRuntime({
      sessions: [summary()],
      detail: detail('local', [
        {
          id: 'm1',
          role: 'user',
          content: '今天有点累',
          persona_uid: 'rama-0001',
          created_at: NOW - 40000,
          is_proactive: false,
          sender_ref: null,
          sender_name: null,
          recalled_at: null,
        },
      ]),
    })

    const { wrapper } = await mountFrom({})
    expect(wrapper.find('.head-actions .btn--ghost').exists()).toBe(false)
    wrapper.unmount()
  })
})

describe('视图状态', () => {
  async function mountWith(options: ChatRuntimeOptions): Promise<VueWrapper> {
    installChatRuntime(options)
    const router = makeRouter()
    await router.push('/')
    await router.isReady()
    const wrapper = mount(ChatView, {
      global: { plugins: [createPinia(), router] },
      attachTo: document.body,
    })
    await flushPromises()
    return wrapper
  }

  it('首屏在途显示加载态与忙碌标记', async () => {
    const wrapper = await mountWith({
      sessions: [summary()],
      commands: { get_session: () => new Promise(() => {}) },
    })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('loading')
    expect(wrapper.find('.chat').attributes('aria-busy')).toBe('true')
    wrapper.unmount()
  })

  it('无会话时为空态，输入区保持可用', async () => {
    const wrapper = await mountWith({ sessions: [] })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('empty')
    expect(wrapper.text()).toContain('还没有消息')
    expect(wrapper.find('textarea').exists()).toBe(true)
    expect(wrapper.find('textarea').attributes('disabled')).toBeUndefined()
    wrapper.unmount()
  })

  it('读取会话失败显示错误态并禁用输入', async () => {
    const wrapper = await mountWith({
      sessions: [summary()],
      commands: {
        get_session: () => {
          throw new Error('会话读取失败')
        },
      },
    })

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('error')
    expect(wrapper.text()).toContain('会话读取失败')
    expect(wrapper.find('textarea').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('隐私未确认时给出隐私降级条与降级标记', async () => {
    const wrapper = await mountWith({ privacy: 'NeedsConfirmation' })

    expect(wrapper.find('.chat').attributes('data-degraded')).toBe('privacy')
    expect(wrapper.text()).toContain('去确认')
    wrapper.unmount()
  })
})
