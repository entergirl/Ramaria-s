/**
 * tests/errorView.test.ts — 不可恢复错误视图
 *
 * 覆盖:
 * - 错误详情：标题 / 说明 / 后端短句与唯一重试动作，不渲染业务内容
 * - 重试中：根节点切到加载态、按钮带加载标记，结束后回到错误态
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import { setTauriRuntime } from '../src/core/transport'
import { useAppStore } from '../src/stores/app'
import ErrorView from '../src/views/error/index.vue'

const Blank = { template: '<div />' }

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/error', name: 'error', component: Blank },
    ],
  })
}

/** 状态读取挂起，用于观察重试中的加载态 */
function installPendingRuntime(): { release: (state: string) => void } {
  let resolveState: (value: string) => void = () => {}
  const pending = new Promise<string>((resolve) => {
    resolveState = resolve
  })

  setTauriRuntime({
    core: {
      invoke: async (command: string) => {
        switch (command) {
          case 'get_app_state':
            return await pending
          case 'get_index_status':
            return {
              status: 'ready',
              loaded: 0,
              total: 0,
              failed: 0,
              percent: 100,
              updated_at: 0,
            }
          case 'get_degraded_reason':
            return null
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

  return { release: (state: string) => resolveState(state) }
}

async function mountError(): Promise<VueWrapper> {
  const pinia = createPinia()
  setActivePinia(pinia)
  useAppStore().bootError = '数据库被锁定'
  const router = makeRouter()
  await router.push('/error')
  await router.isReady()
  const wrapper = mount(ErrorView, { global: { plugins: [pinia, router] } })
  await flushPromises()
  return wrapper
}

afterEach(() => {
  setTauriRuntime(null)
})

describe('错误详情', () => {
  it('渲染标题、说明与后端短句，只留一个重试动作', async () => {
    const wrapper = await mountError()

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('error')
    expect(wrapper.text()).toContain('无法继续')
    expect(wrapper.text()).toContain('数据库被锁定')
    // 全屏兜底不渲染业务内容：只有重试一个可点动作
    expect(wrapper.findAll('button').filter((node) => node.text().includes('重试'))).toHaveLength(1)
    wrapper.unmount()
  })
})

describe('重试', () => {
  it('重试期间切到加载态，结束后回到错误态', async () => {
    const runtime = installPendingRuntime()
    const wrapper = await mountError()

    const retry = wrapper.findAll('button').find((node) => node.text().includes('重试'))
    await retry?.trigger('click')
    await flushPromises()

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('loading')
    expect(wrapper.find('[data-view-state]').attributes('aria-busy')).toBe('true')
    expect(retry?.attributes('data-loading')).toBeDefined()

    runtime.release('fatal_error')
    await flushPromises()

    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('error')
    wrapper.unmount()
  })
})
