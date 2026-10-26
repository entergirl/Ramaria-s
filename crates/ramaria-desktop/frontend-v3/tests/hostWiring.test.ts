/**
 * tests/hostWiring.test.ts — 宿主接线（通知点击定位 / 关闭确认）
 *
 * 覆盖:
 * - 通知点击（`activated`）在非对话视图时切到对话页并携带一次性定位参数
 * - `close-requested` 弹出不可关闭的二选一弹窗，两个动作各调用一次关闭命令
 * - Esc 与遮罩点击不关闭该弹窗（后端 5 秒兜底隐藏到托盘）
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import App from '../src/app/App.vue'
import { setTauriRuntime } from '../src/core/transport'
import { useChatStore } from '../src/stores/chat'
import { callsOf, installChatRuntime, type ChatRuntime } from './helpers/chatRuntime'

const Blank = { template: '<div />' }

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/memory', name: 'memory', component: Blank },
    ],
  })
}

async function mountApp(pinia: Pinia, path = '/memory'): Promise<{ wrapper: VueWrapper; router: Router }> {
  const router = makeRouter()
  await router.push(path)
  await router.isReady()
  const wrapper = mount(App, {
    global: {
      plugins: [pinia, router],
      stubs: { NavDock: true, DownloadBanner: true, ToastLayer: true },
    },
    attachTo: document.body,
  })
  await flushPromises()
  return { wrapper, router }
}

function freshPinia(): Pinia {
  const pinia = createPinia()
  setActivePinia(pinia)
  return pinia
}

function proactivePayload(activated = true): Record<string, unknown> {
  return {
    session_id: 's9',
    message_id: 'm9',
    content: '在忙吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1_700_000_000_000,
    ...(activated ? { activated: true } : {}),
  }
}

function buttonByText(text: string): HTMLButtonElement {
  const hit = Array.from(document.body.querySelectorAll('button')).find((item) =>
    (item.textContent ?? '').includes(text),
  )
  if (!hit) {
    throw new Error(`未找到按钮：${text}`)
  }
  return hit
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('通知点击定位', () => {
  it('非对话视图收到点击重播时切到对话并携带会话参数', async () => {
    const runtime: ChatRuntime = installChatRuntime()
    const { router } = await mountApp(freshPinia())

    runtime.emit('proactive-message', proactivePayload())
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('chat')
    expect(router.currentRoute.value.query).toEqual({
      sessionId: 's9',
      fromView: 'proactive',
    })
  })

  it('已在对话视图时不重复跳转', async () => {
    const runtime = installChatRuntime()
    const pinia = freshPinia()
    const { router } = await mountApp(pinia, '/')

    runtime.emit('proactive-message', proactivePayload())
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('chat')
    expect(router.currentRoute.value.query).toEqual({})
    expect(useChatStore().activation).toEqual({ sessionId: 's9', messageId: 'm9' })
  })
})

describe('关闭确认', () => {
  it('close-requested 弹出二选一，最小化调用一次关闭命令', async () => {
    const runtime = installChatRuntime()
    const { wrapper } = await mountApp(freshPinia())

    runtime.emit('close-requested', { action: 'close-requested' })
    await flushPromises()
    expect(document.body.textContent).toContain('关闭 Ramaria')

    buttonByText('最小化到托盘').click()
    await flushPromises()

    expect(callsOf(runtime, 'confirm_close_action')).toEqual([{ action: 'minimize' }])
    expect(document.body.textContent).not.toContain('关闭 Ramaria')
    wrapper.unmount()
  })

  it('退出动作调用 exit，且 Esc 与遮罩点击不关闭弹窗', async () => {
    const runtime = installChatRuntime()
    await mountApp(freshPinia())

    runtime.emit('close-requested', { action: 'close-requested' })
    await flushPromises()

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    document.body.querySelector('.confirm-veil')?.dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    await flushPromises()
    expect(document.body.textContent).toContain('关闭 Ramaria')

    buttonByText('退出 Ramaria').click()
    await flushPromises()
    expect(callsOf(runtime, 'confirm_close_action')).toEqual([{ action: 'exit' }])
  })
})
