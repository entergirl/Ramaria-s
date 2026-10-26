/**
 * tests/settingsView.test.ts — 设置视图的加载、保存与回滚
 *
 * 覆盖:
 * - 加载后按目录渲染卡片并回显生效配置
 * - 保存只覆盖本卡字段，且以保存前重新读取的最新配置为基底
 * - 恢复默认只改表单（不写库），再保存才落库
 * - 深链在进阶未解锁时收敛回基础页，解锁后可直达进阶子页
 * - 主请求失败 → 视图错误 + 重试；单卡双侧写失败 → 卡内错误且保留草稿
 */

import { flushPromises, mount, type DOMWrapper, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type { ConfigWriteReceipt } from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { useDebugStore } from '../src/stores/debug'
import { useNavStore } from '../src/stores/nav'
import SettingsView from '../src/views/settings/index.vue'

interface RecordedCall {
  command: string
  args: Record<string, unknown>
}

interface RuntimeConfig {
  full: Record<string, unknown>
  defaults: Record<string, unknown>
}

function sampleFull(): Record<string, unknown> {
  return {
    version: '3.0.0',
    schema_version: 3,
    backend: {
      provider: 'lm_studio',
      model_id: '',
      base_url: 'http://localhost:1234/v1',
      embedding_model_id: null,
      temperature: 0.3,
      max_tokens: 4096,
      chat_thinking: true,
      role_immersion: 'system',
      chat_temperature: null,
    },
    embedding: { device: 'auto' },
    vision: { model_supports_vision: false, batch_limit: 0 },
    session: {
      l1_idle_minutes: 30,
      idle_check_interval_seconds: 60,
      l2_check_interval_seconds: 86400,
      max_history_messages: 200,
      max_history_chars: 6000,
    },
    proactive: { enabled: true },
    retrieval: { rrf_k: 60 },
  }
}

function sampleDefaults(): Record<string, unknown> {
  const defaults = sampleFull()
  defaults.session = {
    l1_idle_minutes: 10,
    idle_check_interval_seconds: 60,
    l2_check_interval_seconds: 86400,
    max_history_messages: 200,
    max_history_chars: 6000,
  }
  return defaults
}

function makeRuntime(config: RuntimeConfig) {
  const calls: RecordedCall[] = []
  const state = {
    full: structuredClone(config.full),
    latest: null as Record<string, unknown> | null,
    failFullOnce: false,
    receipt: { fileOk: true, dbOk: true, failures: [] } as ConfigWriteReceipt,
    written: null as Record<string, unknown> | null,
    apiKey: null as string | null,
    embeddingPath: null as string | null,
  }

  const runtime: TauriRuntime = {
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        switch (command) {
          case 'get_full_config': {
            if (state.failFullOnce) {
              state.failFullOnce = false
              throw new Error('数据库读取失败')
            }
            return JSON.stringify(state.latest ?? state.full)
          }
          case 'get_default_config':
            return JSON.stringify(config.defaults)
          case 'update_full_config': {
            const next = JSON.parse(String(args.configJson)) as Record<string, unknown>
            state.written = next
            if (state.receipt.fileOk || state.receipt.dbOk) {
              state.full = next
              state.latest = next
            }
            return state.receipt
          }
          case 'update_backend_config': {
            state.apiKey = args.apiKey ? String(args.apiKey) : null
            return 'ok'
          }
          case 'save_embedding_model': {
            state.embeddingPath = String(args.path ?? '') || null
            return 'ok'
          }
          case 'get_backend_config':
            return {
              provider: 'lm_studio',
              modelId: '',
              baseUrl: 'http://localhost:1234/v1',
              supportsStreaming: true,
              supportsJsonMode: true,
              contextWindow: 8192,
              maxOutputTokens: 4096,
              apiKeyMasked: state.apiKey ? 'sk-****' : null,
            }
          case 'get_embedding_model':
            return state.embeddingPath
              ? { model_path: state.embeddingPath, valid: true, dimension: 512 }
              : null
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
  }

  return { runtime, calls, state }
}

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: { template: '<div />' } },
      { path: '/settings/:scope?/:section?', name: 'settings', component: { template: '<div />' } },
    ],
  })
}

type ButtonHost = VueWrapper | DOMWrapper<Element>

function buttonByText(host: ButtonHost, text: string) {
  const button = host.findAll('button').find((item) => item.text() === text)
  if (!button) {
    throw new Error(`未找到按钮: ${text}`)
  }
  return button
}

async function mountSettings(path: string, runtimeConfig: RuntimeConfig = {
  full: sampleFull(),
  defaults: sampleDefaults(),
}) {
  const { runtime, calls, state } = makeRuntime(runtimeConfig)
  setTauriRuntime(runtime)
  const pinia = createPinia()
  setActivePinia(pinia)
  const router = makeRouter()
  await router.push(path)
  await router.isReady()
  const wrapper = mount(SettingsView, { global: { plugins: [pinia, router] } })
  await flushPromises()
  await nextTick()
  return { wrapper, calls, state, router }
}

beforeEach(() => {
  window.localStorage.clear()
})

afterEach(() => {
  window.localStorage.clear()
})

describe('设置视图', () => {
  it('加载后渲染当前子页的卡片并回显配置', async () => {
    const { wrapper } = await mountSettings('/settings/basic/model')

    expect(wrapper.find('.settings').attributes('data-view-state')).toBe('default')
    expect(wrapper.find('[data-card="backend"]').exists()).toBe(true)
    expect(wrapper.find('[data-card="embedding"]').exists()).toBe(true)
    const input = wrapper.find('[data-field="backend.base_url"] input')
    expect((input.element as HTMLInputElement).value).toBe('http://localhost:1234/v1')
  })

  it('界面与导航卡按方位缩略图切换预设，即写入本地偏好', async () => {
    const { wrapper } = await mountSettings('/settings/basic/data')
    const card = wrapper.findAll('.settings-card').find((node) => node.text().includes('界面与导航'))
    expect(card).toBeTruthy()
    expect(card?.findAll('[data-pos]')).toHaveLength(5)

    await card?.find('[data-pos="top-left"] input').setValue(true)
    await flushPromises()
    expect(useNavStore().preset).toBe('top-left')
    expect(card?.find('[data-pos="top-left"]').classes()).toContain('is-active')
    expect(window.localStorage.getItem('ramaria.navPosition')).toBe('top-left')
  })

  it('字段改动后分类键出现未保存徽标，保存后清除', async () => {
    const { wrapper } = await mountSettings('/settings/basic/model')
    const category = () =>
      wrapper.findAll('.chip-nav-item').find((item) => item.text().includes('模型与检索'))

    const input = wrapper.find('[data-field="backend.base_url"] input')
    await input.setValue('http://127.0.0.1:9999/v1')
    await flushPromises()
    expect(category()?.find('.chip-nav-count').text()).toBe('1')

    await buttonByText(wrapper.find('[data-card="backend"]'), '保存').trigger('click')
    await flushPromises()
    expect(category()?.find('.chip-nav-count').exists()).toBe(false)
  })

  it('只改密钥时由卡片保存直接写入，不触发配置回写', async () => {
    const full = sampleFull()
    ;(full.backend as Record<string, unknown>).provider = 'deepseek'
    const { wrapper, calls, state } = await mountSettings('/settings/basic/model', {
      full,
      defaults: sampleDefaults(),
    })

    const keyInput = wrapper.find('[data-card="backend"] input[type="password"]')
    expect(keyInput.exists()).toBe(true)
    expect((keyInput.element as HTMLInputElement).disabled).toBe(false)
    await keyInput.setValue('  sk-test  ')
    await flushPromises()
    const saveButton = buttonByText(wrapper.find('[data-card="backend"]'), '保存')
    expect(saveButton.attributes('disabled')).toBeUndefined()
    await saveButton.trigger('click')
    await flushPromises()

    expect(state.apiKey).toBe('sk-test')
    expect(calls.some((call) => call.command === 'update_full_config')).toBe(false)
    expect((keyInput.element as HTMLInputElement).value).toBe('')
  })

  it('模型路径随卡片保存热加载，页脚保留校验动作', async () => {
    const { wrapper, calls, state } = await mountSettings('/settings/basic/model')
    const card = wrapper.find('[data-card="embedding"]')
    expect(card.text()).toContain('校验向量模型')
    expect(card.text()).not.toContain('刷新状态')

    const pathInput = card.find('input[placeholder="留空 = 卸载"]')
    await pathInput.setValue('D:/models/other')
    await buttonByText(card, '保存').trigger('click')
    await flushPromises()

    expect(state.embeddingPath).toBe('D:/models/other')
    expect(calls.some((call) => call.command === 'update_full_config')).toBe(false)
  })

  it('保存只覆盖本卡字段，并以最新配置为基底', async () => {
    const { wrapper, calls, state } = await mountSettings('/settings/basic/model')
    const latest = sampleFull()
    ;(latest.session as Record<string, unknown>).l1_idle_minutes = 99
    ;(latest.backend as Record<string, unknown>).base_url = 'http://stale'
    state.latest = latest

    const input = wrapper.find('[data-field="backend.base_url"] input')
    await input.setValue('http://127.0.0.1:8080/v1')
    await buttonByText(wrapper.find('[data-card="backend"]'), '保存').trigger('click')
    await flushPromises()

    const write = calls.find((call) => call.command === 'update_full_config')
    expect(write).toBeTruthy()
    const written = state.written as Record<string, unknown>
    expect((written.backend as Record<string, unknown>).base_url).toBe('http://127.0.0.1:8080/v1')
    expect((written.session as Record<string, unknown>).l1_idle_minutes).toBe(99)
    expect(wrapper.find('[data-card="backend"]').text()).toContain('已保存')
  })

  it('恢复默认只改表单，再保存才写库', async () => {
    const { wrapper, calls, state } = await mountSettings('/settings/basic/privacy')
    const sessionCard = wrapper.find('[data-card="session"]')
    const input = sessionCard.find('[data-field="session.l1_idle_minutes"] input')
    expect((input.element as HTMLInputElement).value).toBe('30')

    await buttonByText(sessionCard, '恢复默认').trigger('click')
    await flushPromises()

    expect((input.element as HTMLInputElement).value).toBe('10')
    expect(calls.some((call) => call.command === 'update_full_config')).toBe(false)
    expect(sessionCard.text()).toContain('有未保存的修改')

    await buttonByText(sessionCard, '保存').trigger('click')
    await flushPromises()
    const written = state.written as Record<string, unknown>
    expect((written.session as Record<string, unknown>).l1_idle_minutes).toBe(10)
  })

  it('进阶未解锁时深链收敛回基础页，解锁后可直达', async () => {
    const { wrapper, router } = await mountSettings('/settings/advanced/adv-retrieval')

    expect(router.currentRoute.value.params.scope).toBe('basic')
    expect(router.currentRoute.value.params.section).toBe('model')
    expect(wrapper.find('[data-card="backend"]').exists()).toBe(true)

    const debug = useDebugStore()
    debug.setUnlocked(true)
    await router.push('/settings/advanced/adv-retrieval')
    await flushPromises()
    await nextTick()

    expect(router.currentRoute.value.params.section).toBe('adv-retrieval')
    expect(wrapper.find('[data-card="retrieval"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('检索参数')
  })

  it('主请求失败显示视图错误，重试后恢复', async () => {
    const failing = makeRuntime({ full: sampleFull(), defaults: sampleDefaults() })
    failing.state.failFullOnce = true
    setTauriRuntime(failing.runtime)
    const pinia = createPinia()
    setActivePinia(pinia)
    const router = makeRouter()
    await router.push('/settings/basic/model')
    await router.isReady()
    const wrapper = mount(SettingsView, { global: { plugins: [pinia, router] } })
    await flushPromises()

    expect(wrapper.find('.settings').attributes('data-view-state')).toBe('error')
    expect(wrapper.text()).toContain('设置加载失败')

    await buttonByText(wrapper, '重试').trigger('click')
    await flushPromises()
    expect(wrapper.find('.settings').attributes('data-view-state')).toBe('default')
    expect(failing.calls.filter((call) => call.command === 'get_full_config')).toHaveLength(2)
  })

  it('单卡双侧写失败：错误留在卡内并保留草稿', async () => {
    const { wrapper, state } = await mountSettings('/settings/basic/model')
    state.receipt = { fileOk: false, dbOk: false, failures: ['磁盘写入失败'] }

    const input = wrapper.find('[data-field="backend.base_url"] input')
    await input.setValue('http://127.0.0.1:9999/v1')
    await buttonByText(wrapper.find('[data-card="backend"]'), '保存').trigger('click')
    await flushPromises()

    const card = wrapper.find('[data-card="backend"]')
    expect(card.text()).toContain('磁盘写入失败')
    expect((input.element as HTMLInputElement).value).toBe('http://127.0.0.1:9999/v1')
    expect(card.text()).toContain('未保存')
    expect(state.written).not.toBeNull()
  })
})
