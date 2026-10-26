/**
 * tests/appRouting.test.ts — 应用状态机路由与全屏守卫
 *
 * 覆盖:
 * - 守卫：`needs_setup` / `fatal_error` 把任意主视图拦回对应全屏页
 * - 跟随：`downloading_model` 不切视图，`ready` / `degraded` 留在上次导航视图
 * - 首次配置完成写回 `ready` 后离开全屏页
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import App from '../src/app/App.vue'
import { createAppRouter } from '../src/app/router'
import type { AppStateValue, DegradedReason } from '../src/core/api/dto'
import { setTauriRuntime } from '../src/core/transport'
import { useAppStore } from '../src/stores/app'

const Blank = { template: '<div />' }

/** 安装最小宿主替身：只应答启动所需的两条状态命令 */
function installRuntime(): void {
  setTauriRuntime({
    core: {
      invoke: async (command: string) => {
        switch (command) {
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
}

interface BootOptions {
  state: AppStateValue
  reason?: DegradedReason | null
}

/** 以给定状态创建应用 store（已引导） */
function freshStore(options: BootOptions): { pinia: Pinia; app: ReturnType<typeof useAppStore> } {
  const pinia = createPinia()
  setActivePinia(pinia)
  const app = useAppStore()
  app.booted = true
  app.appState = options.state
  app.degradedReason = options.reason ?? null
  return { pinia, app }
}

/** 简易路由：用于观察根组件的状态跟随，不加载真实视图 */
function blankRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/memory', name: 'memory', component: Blank },
      { path: '/setup', name: 'setup', component: Blank },
      { path: '/error', name: 'error', component: Blank },
    ],
  })
}

async function mountApp(pinia: Pinia, router: Router, path: string): Promise<VueWrapper> {
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
  return wrapper
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('全屏守卫', () => {
  it('needs_setup 时主视图被拦回配置页', async () => {
    installRuntime()
    freshStore({ state: 'needs_setup' })
    const router = createAppRouter()
    await router.push('/memory')
    await router.isReady()

    expect(router.currentRoute.value.name).toBe('setup')
  })

  it('fatal_error 时主视图被拦到错误页', async () => {
    installRuntime()
    freshStore({ state: 'fatal_error' })
    const router = createAppRouter()
    await router.push('/chat')
    await router.isReady()

    expect(router.currentRoute.value.name).toBe('error')
  })
})

describe('状态跟随', () => {
  it('downloading_model 不切视图', async () => {
    installRuntime()
    const { pinia } = freshStore({ state: 'downloading_model' })
    const router = blankRouter()
    const wrapper = await mountApp(pinia, router, '/memory')

    expect(router.currentRoute.value.name).toBe('memory')
    wrapper.unmount()
  })

  it('ready 停留在上次导航视图', async () => {
    installRuntime()
    const { pinia } = freshStore({ state: 'ready' })
    const router = blankRouter()
    const wrapper = await mountApp(pinia, router, '/memory')

    expect(router.currentRoute.value.name).toBe('memory')
    wrapper.unmount()
  })

  it('degraded 停留在上次导航视图并带上降级原因', async () => {
    installRuntime()
    const { pinia, app } = freshStore({ state: 'degraded', reason: 'llm_unavailable' })
    const router = blankRouter()
    const wrapper = await mountApp(pinia, router, '/memory')

    expect(router.currentRoute.value.name).toBe('memory')
    expect(app.degradedReason).toBe('llm_unavailable')
    wrapper.unmount()
  })

  it('首次配置完成写 ready 后离开全屏页', async () => {
    installRuntime()
    const { pinia, app } = freshStore({ state: 'needs_setup' })
    const router = blankRouter()
    const wrapper = await mountApp(pinia, router, '/setup')

    app.appState = 'ready'
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('chat')
    wrapper.unmount()
  })
})
