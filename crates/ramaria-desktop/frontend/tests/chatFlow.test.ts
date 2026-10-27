/**
 * tests/chatFlow.test.ts — 对话视图的关键交互
 *
 * 覆盖:
 * - 发送后进入流式：输入锁定、增量上屏、完成后恢复并可继续输入
 * - 发送失败给出失败气泡与重试，重试复用原文
 * - 撤回需二次确认，成功后折叠为系统条
 * - 发图走文件对话框与本地发图命令，图片消息落到列表
 * - 横幅优先级：只读高于索引与模型不可用；索引未建立与模型不可用各自出条
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type { IndexStatusView } from '../src/core/api/dto'
import { setTauriRuntime } from '../src/core/transport'
import { useAppStore } from '../src/stores/app'
import ChatView from '../src/views/chat/index.vue'
import {
  RAMA,
  SELF,
  callsOf,
  detail,
  installChatRuntime,
  message,
  summary,
} from './helpers/chatRuntime'

const Blank = { template: '<div />' }

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/settings', name: 'settings', component: Blank },
    ],
  })
}

async function mountChat(pinia: Pinia): Promise<VueWrapper> {
  const router = makeRouter()
  await router.push('/')
  await router.isReady()
  const wrapper = mount(ChatView, {
    global: { plugins: [pinia, router] },
    attachTo: document.body,
  })
  await flushPromises()
  return wrapper
}

function freshPinia(): Pinia {
  const pinia = createPinia()
  setActivePinia(pinia)
  return pinia
}

function indexStatus(patch: Partial<IndexStatusView> = {}): IndexStatusView {
  return {
    status: 'idle',
    loaded: 0,
    total: 0,
    failed: 0,
    percent: 0,
    updated_at: 0,
    ...patch,
  }
}

beforeAll(() => {
  Object.defineProperty(URL, 'createObjectURL', { value: () => 'blob:mock', writable: true })
  Object.defineProperty(URL, 'revokeObjectURL', { value: () => {}, writable: true })
})

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('流式生成', () => {
  it('发送后锁定输入，增量上屏，完成后恢复', async () => {
    let serverDetail = detail('local', [message()])
    const runtime = installChatRuntime({
      commands: {
        send_message: () => 'req-1',
        get_session: () => serverDetail,
      },
    })
    const wrapper = await mountChat(freshPinia())

    await wrapper.find('textarea').setValue('晚上吃什么')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()

    expect(callsOf(runtime, 'send_message')[0]).toMatchObject({
      message: '晚上吃什么',
      personaUid: 'rama-0001',
      sessionId: 's1',
    })
    expect(wrapper.text()).toContain('晚上吃什么')

    runtime.emit('chat-delta', { request_id: 'req-1', content: '先去吃饭||别饿着' })
    await flushPromises()
    expect(wrapper.find('[data-restricted]').attributes('data-restricted')).toBe('locked')
    expect(wrapper.find('.chat').attributes('aria-busy')).toBe('true')
    expect(wrapper.find('textarea').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('先去吃饭')

    serverDetail = detail('local', [
      message(),
      message({ id: 'u2', content: '晚上吃什么' }),
      message({ id: 'a1', role: 'assistant', content: '先去吃饭||别饿着' }),
    ])
    runtime.emit('chat-done', {
      request_id: 'req-1',
      backend_id: 'lmstudio',
      total_chars: 11,
      content: '先去吃饭||别饿着',
    })
    await flushPromises()

    expect(wrapper.find('[data-restricted]').exists()).toBe(false)
    expect(wrapper.find('.chat').attributes('aria-busy')).toBeUndefined()
    expect(wrapper.find('textarea').attributes('disabled')).toBeUndefined()
    expect(wrapper.text()).toContain('别饿着')
    expect(wrapper.findAll('.bubble').length).toBeGreaterThanOrEqual(3)
    wrapper.unmount()
  })

  it('发送失败给出失败气泡，重试复用原文', async () => {
    const runtime = installChatRuntime({
      commands: {
        send_message: () => {
          throw new Error('对话模型未就绪')
        },
      },
    })
    const wrapper = await mountChat(freshPinia())

    await wrapper.find('textarea').setValue('在吗')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('对话模型未就绪')
    const retry = wrapper.find('.bubble-retry')
    expect(retry.exists()).toBe(true)

    await retry.trigger('click')
    await flushPromises()
    expect(callsOf(runtime, 'send_message')).toHaveLength(2)
    expect(callsOf(runtime, 'send_message')[1]).toMatchObject({ message: '在吗' })
    wrapper.unmount()
  })

  it('流式增量只刷新流式气泡，不重建已有消息节点', async () => {
    const runtime = installChatRuntime({
      sessionDetail: detail('local', [
        message({ id: 'm1' }),
        message({ id: 'm2', role: 'assistant', content: '在的' }),
      ]),
      commands: { send_message: () => 'req-1' },
    })
    const wrapper = await mountChat(freshPinia())

    const before = wrapper.findAll('.msg-row').map((node) => node.element)
    expect(before.length).toBeGreaterThan(0)

    await wrapper.find('textarea').setValue('在吗')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()
    runtime.emit('chat-delta', { request_id: 'req-1', content: '先' })
    await flushPromises()
    runtime.emit('chat-delta', { request_id: 'req-1', content: '说一句' })
    await flushPromises()

    const after = wrapper.findAll('.msg-row')
    before.forEach((element, index) => {
      expect(after[index]?.element).toBe(element)
    })
    expect(wrapper.text()).toContain('先说一句')
    wrapper.unmount()
  })

  it('非本次请求的增量被忽略', async () => {
    const runtime = installChatRuntime({ commands: { send_message: () => 'req-1' } })
    const wrapper = await mountChat(freshPinia())

    await wrapper.find('textarea').setValue('在吗')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()

    runtime.emit('chat-delta', { request_id: 'req-other', content: '串流内容' })
    await flushPromises()
    expect(wrapper.text()).not.toContain('串流内容')

    runtime.emit('chat-delta', { request_id: 'req-1', content: '在的' })
    await flushPromises()
    expect(wrapper.text()).toContain('在的')
    wrapper.unmount()
  })
})

describe('主动消息', () => {
  it('当前会话追加带主动标识的消息，同标识不重复', async () => {
    const runtime = installChatRuntime()
    const wrapper = await mountChat(freshPinia())

    const payload = {
      session_id: 's1',
      message_id: 'p1',
      content: '今天降温了，记得加衣',
      persona: 'rama-0001',
      source: 'proactive',
      created_at: Date.now(),
    }
    runtime.emit('proactive-message', payload)
    await flushPromises()
    runtime.emit('proactive-message', payload)
    await flushPromises()

    expect(wrapper.text()).toContain('今天降温了')
    expect(wrapper.text()).toContain('主动消息')
    expect(wrapper.findAll('.proactive-tag')).toHaveLength(1)
    wrapper.unmount()
  })
})

describe('到达即已读', () => {
  it('回复到达后推进已读时间', async () => {
    const runtime = installChatRuntime({ commands: { send_message: () => 'req-1' } })
    const wrapper = await mountChat(freshPinia())
    const before = callsOf(runtime, 'mark_session_read').length

    await wrapper.find('textarea').setValue('在吗')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()
    runtime.emit('chat-done', {
      request_id: 'req-1',
      backend_id: 'lmstudio',
      total_chars: 2,
      content: '在的',
    })
    await flushPromises()

    const calls = callsOf(runtime, 'mark_session_read')
    expect(calls.length).toBe(before + 1)
    expect(calls.at(-1)).toEqual({ sessionId: 's1' })
    wrapper.unmount()
  })

  it('当前会话收到主动消息后推进已读时间', async () => {
    const runtime = installChatRuntime()
    const wrapper = await mountChat(freshPinia())
    const before = callsOf(runtime, 'mark_session_read').length

    runtime.emit('proactive-message', {
      session_id: 's1',
      message_id: 'p2',
      content: '记得喝水',
      persona: 'rama-0001',
      source: 'proactive',
      created_at: Date.now(),
    })
    await flushPromises()

    const calls = callsOf(runtime, 'mark_session_read')
    expect(calls.length).toBe(before + 1)
    expect(calls.at(-1)).toEqual({ sessionId: 's1' })
    wrapper.unmount()
  })
})

describe('撤回', () => {
  it('本人消息可撤回，确认后折叠为系统条', async () => {
    const runtime = installChatRuntime({
      sessionDetail: detail('local', [message({ id: 'm1', created_at: Date.now() - 5_000 })]),
      commands: { recall_message: () => null },
    })
    const wrapper = await mountChat(freshPinia())

    const recall = wrapper.find('.recall-btn')
    expect(recall.exists()).toBe(true)
    await recall.trigger('click')
    await flushPromises()

    const confirm = Array.from(
      document.body.querySelectorAll<HTMLButtonElement>('.modal-card button'),
    ).find((button) => button.textContent?.trim() === '撤回')
    expect(confirm).toBeTruthy()
    confirm?.click()
    await flushPromises()

    expect(callsOf(runtime, 'recall_message')[0]).toEqual({ messageId: 'm1' })
    expect(wrapper.find('.msg-recalled').text()).toContain('你撤回了一条消息')
    wrapper.unmount()
  })

  it('超窗消息不渲染撤回入口', async () => {
    installChatRuntime({
      sessionDetail: detail('local', [message({ id: 'm1', created_at: Date.now() - 300_000 })]),
    })
    const wrapper = await mountChat(freshPinia())
    expect(wrapper.find('.recall-btn').exists()).toBe(false)
    wrapper.unmount()
  })
})

describe('发图', () => {
  it('选文件后落库并渲染图片消息', async () => {
    const runtime = installChatRuntime({
      commands: {
        get_image_source_dir: () => 'C:/outgoing',
        open_file_dialog: () => 'C:/outgoing/cat.png',
        send_local_image: () => ({
          messageId: 'img-1',
          placeholder: '[图片#abcd1234]',
          content: '[图片#abcd1234]',
        }),
        read_attachment_image: () => new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer,
      },
    })
    const wrapper = await mountChat(freshPinia())

    await wrapper.find('.composer .attach-btn').trigger('click')
    await flushPromises()

    expect(callsOf(runtime, 'open_file_dialog')[0]).toMatchObject({
      initialDir: 'C:/outgoing',
      extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'],
    })
    expect(callsOf(runtime, 'send_local_image')[0]).toMatchObject({
      sessionId: 's1',
      filePath: 'C:/outgoing/cat.png',
    })
    // 图片消息进入列表后按消息标识与序号读取字节（jsdom 不触发图片解码，只校验读取入参）
    expect(callsOf(runtime, 'read_attachment_image')[0]).toEqual({ messageId: 'img-1', index: 0 })
    wrapper.unmount()
  })
})

describe('横幅', () => {
  it('索引未建立时给出降级条与重建动作', async () => {
    const runtime = installChatRuntime({
      commands: {
        rebuild_index: () => ({ started: true }),
        get_index_status: () => indexStatus({ status: 'building', percent: 8 }),
      },
    })
    const pinia = freshPinia()
    useAppStore().indexStatus = indexStatus({ status: 'idle' })
    const wrapper = await mountChat(pinia)

    const banner = wrapper.find('.banner')
    expect(banner.attributes('data-kind')).toBe('index-idle')
    expect(banner.text()).toContain('索引未建立，召回降级')

    await banner.find('.btn').trigger('click')
    await flushPromises()
    expect(callsOf(runtime, 'rebuild_index')).toHaveLength(1)
    wrapper.unmount()
  })

  it('只读会话优先于索引提示', async () => {
    installChatRuntime({
      sessions: [summary({ id: 's2', channel: 'qq', persona_uid: 'rama-0001' })],
      sessionDetail: detail('qq', [message({ sender_name: '小珊', sender_ref: '10001' })]),
    })
    const pinia = freshPinia()
    useAppStore().indexStatus = indexStatus({ status: 'idle' })
    const wrapper = await mountChat(pinia)

    await wrapper.find('.avatar').trigger('click')
    await flushPromises()
    await wrapper.find('.session-row').trigger('click')
    await flushPromises()

    const banner = wrapper.find('.banner')
    expect(banner.attributes('data-kind')).toBe('readonly')
    expect(banner.text()).not.toContain('索引未建立')
    expect(wrapper.find('[data-restricted]').attributes('data-restricted')).toBe('readonly')
    wrapper.unmount()
  })

  it('只读会话与模型不可用并存：只读横幅优先，降级标记仍记录模型侧', async () => {
    installChatRuntime({
      sessions: [summary({ id: 's2', channel: 'qq', persona_uid: 'rama-0001' })],
      sessionDetail: detail('qq', [message({ sender_name: '小珊', sender_ref: '10001' })]),
    })
    const pinia = freshPinia()
    const app = useAppStore()
    app.appState = 'degraded'
    app.degradedReason = 'llm_unavailable'
    const wrapper = await mountChat(pinia)

    await wrapper.find('.avatar').trigger('click')
    await flushPromises()
    await wrapper.find('.session-row').trigger('click')
    await flushPromises()

    expect(wrapper.find('.banner').attributes('data-kind')).toBe('readonly')
    expect(wrapper.find('[data-degraded]').attributes('data-degraded')).toContain('llm')
    expect(wrapper.find('[data-restricted]').attributes('data-restricted')).toBe('readonly')
    wrapper.unmount()
  })

  it('模型不可用时给出降级条与降级标记', async () => {
    installChatRuntime()
    const pinia = freshPinia()
    const app = useAppStore()
    app.appState = 'degraded'
    app.degradedReason = 'llm_unavailable'
    const wrapper = await mountChat(pinia)

    expect(wrapper.find('.banner').attributes('data-kind')).toBe('llm')
    expect(wrapper.find('[data-degraded]').attributes('data-degraded')).toContain('llm')
    wrapper.unmount()
  })
})

describe('空对话入口', () => {
  const ANIM = {
    uid: 'anim-0001',
    name: '小雨',
    kind: 'anim',
    source: 'manual',
    ref_id: null,
    avatar: null,
    config: null,
    description: null,
    is_active: true,
    created_at: 0,
    updated_at: 0,
  }

  it('为空人格预留空对话，点击后按该人格新建会话并发送', async () => {
    const runtime = installChatRuntime({
      sessions: [],
      settings: [{ key: 'default_persona_uid', value: 'rama-0001' }],
      sessionDetail: detail('local', []),
      commands: {
        list_personas_full: () => [RAMA, SELF, ANIM],
        create_session: () => summary({ id: 's2', persona_uid: 'anim-0001', message_count: 0 }),
        send_message: () => 'req-draft',
      },
    })
    const wrapper = await mountChat(freshPinia())

    await wrapper.find('.avatar').trigger('click')
    await flushPromises()
    const row = wrapper.findAll('.session-row').find((node) => node.text().includes('小雨'))
    expect(row).toBeTruthy()
    expect(row?.text()).toContain('空对话')

    await row?.trigger('click')
    await flushPromises()
    expect(wrapper.find('.chat-head').text()).toContain('小雨')

    await wrapper.find('textarea').setValue('你好')
    await wrapper.find('.composer .btn--primary').trigger('click')
    await flushPromises()

    expect(callsOf(runtime, 'create_session')[0]).toMatchObject({ personaUid: 'anim-0001' })
    expect(callsOf(runtime, 'send_message')[0]).toMatchObject({
      personaUid: 'anim-0001',
      sessionId: 's2',
    })
    wrapper.unmount()
  })
})
