/**
 * tests/setupView.test.ts — 首次配置视图挂载与流程
 *
 * 覆盖:
 * - 默认落在第 1 步并渲染本地字段与主态标记
 * - 运行方式切换后显示线上字段
 * - 字段校验未通过时不前进
 * - 本地三步走通：测试连接 → 跳过嵌入 → 保存并进入应用
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { afterEach, describe, expect, it } from 'vitest'

import { setTauriRuntime } from '../src/core/transport'
import SetupView from '../src/views/setup/index.vue'

async function mountSetup(): Promise<VueWrapper> {
  const wrapper = mount(SetupView, {
    global: { plugins: [createPinia()] },
    attachTo: document.body,
  })
  await flushPromises()
  return wrapper
}

/** 按可见文案找按钮 */
function buttonByText(wrapper: VueWrapper, text: string) {
  return wrapper.findAll('button').find((button) => button.text().includes(text))
}

afterEach(() => {
  setTauriRuntime(null)
})

describe('首次配置视图', () => {
  it('默认第 1 步并渲染本地字段', async () => {
    const wrapper = await mountSetup()
    expect(wrapper.text()).toContain('第 1 / 3 步')
    expect(wrapper.text()).toContain('推理服务地址')
    expect(wrapper.find('[data-view-state]').attributes('data-view-state')).toBe('default')
    wrapper.unmount()
  })

  it('切到线上 API 后显示密钥与 API 地址字段', async () => {
    const wrapper = await mountSetup()
    const online = wrapper
      .findAll('.mode-card')
      .find((button) => button.text().includes('线上 API'))
    expect(online).toBeTruthy()
    await online?.trigger('click')
    expect(wrapper.text()).toContain('API Key')
    expect(wrapper.text()).toContain('API 地址')
    wrapper.unmount()
  })

  it('模型名为空时下一步被字段校验拦下', async () => {
    const wrapper = await mountSetup()
    await wrapper.find('button.btn--primary').trigger('click')
    expect(wrapper.text()).toContain('请填写模型名称')
    expect(wrapper.text()).toContain('第 1 / 3 步')
    wrapper.unmount()
  })
})

describe('首次配置流程（本地）', () => {
  it('测试连接、跳过嵌入、提交并进入应用', async () => {
    const calls: string[] = []
    setTauriRuntime({
      core: {
        invoke: async (command: string) => {
          calls.push(command)
          switch (command) {
            case 'get_embedding_model':
              return null
            case 'get_full_config':
              return '{"vision":{"model_supports_vision":false,"batch_limit":0}}'
            case 'get_backend_config':
              return {
                provider: 'lmstudio',
                modelId: '',
                baseUrl: 'http://127.0.0.1:1234/v1',
                supportsStreaming: true,
                supportsJsonMode: false,
                contextWindow: 8192,
                maxOutputTokens: 2048,
                apiKeyMasked: null,
              }
            case 'update_backend_config':
            case 'test_llm_connection':
            case 'run_setup':
            case 'refresh_setup_state':
              return command === 'run_setup' || command === 'refresh_setup_state' ? 'ready' : 'ok'
            case 'set_vision_support':
              return { fileOk: true, dbOk: true, failures: [] }
            case 'get_index_status':
              return {
                status: 'ready',
                loaded: 0,
                total: 0,
                failed: 0,
                percent: 100,
                updated_at: 0,
              }
            default:
              throw new Error(`未预期的命令: ${command}`)
          }
        },
      },
    })

    const wrapper = await mountSetup()
    const inputs = wrapper.findAll('.field-input')
    await inputs[1]?.setValue('qwen3-8b')

    await buttonByText(wrapper, '测试连接')?.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('本地推理服务可达')

    await buttonByText(wrapper, '下一步')?.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('配置嵌入模型')

    await buttonByText(wrapper, '跳过，稍后配置')?.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('确认配置')
    expect(wrapper.text()).toContain('降级')

    await buttonByText(wrapper, '保存并进入 Ramaria')?.trigger('click')
    await flushPromises()
    expect(calls).toContain('run_setup')
    expect(calls).toContain('set_vision_support')
    expect(calls).not.toContain('update_full_config')
    expect(calls).not.toContain('update_setting')
    expect(calls).toContain('refresh_setup_state')

    wrapper.unmount()
  })
})

/** 安装首次配置需要的命令替身；`overrides` 覆盖单条命令作答 */
function installSetupRuntime(overrides: Record<string, () => unknown> = {}): void {
  setTauriRuntime({
    core: {
      invoke: async (command: string) => {
        const override = overrides[command]
        if (override) {
          return override()
        }
        switch (command) {
          case 'get_embedding_model':
            return null
          case 'get_full_config':
            return '{"vision":{"model_supports_vision":false,"batch_limit":0}}'
          case 'get_backend_config':
            return {
              provider: 'lmstudio',
              modelId: '',
              baseUrl: 'http://127.0.0.1:1234/v1',
              supportsStreaming: true,
              supportsJsonMode: false,
              contextWindow: 8192,
              maxOutputTokens: 2048,
              apiKeyMasked: null,
            }
          case 'update_backend_config':
          case 'test_llm_connection':
          case 'set_vision_support':
            return 'ok'
          case 'run_setup':
          case 'refresh_setup_state':
            return 'ready'
          case 'get_index_status':
            return {
              status: 'ready',
              loaded: 0,
              total: 0,
              failed: 0,
              percent: 100,
              updated_at: 0,
            }
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
  })
}

/** 填好模型名后点击测试连接 */
async function fillAndTest(wrapper: VueWrapper): Promise<void> {
  await wrapper.findAll('.field-input')[1]?.setValue('qwen3-8b')
  await buttonByText(wrapper, '测试连接')?.trigger('click')
  await flushPromises()
}

describe('步骤内状态', () => {
  it('测试连接在途显示加载文案与按钮加载标记', async () => {
    installSetupRuntime({ test_llm_connection: () => new Promise(() => {}) })
    const wrapper = await mountSetup()

    await fillAndTest(wrapper)

    expect(wrapper.text()).toContain('正在测试连接…')
    const testButton = buttonByText(wrapper, '测试连接')
    expect(testButton?.attributes('data-loading')).toBeDefined()
    wrapper.unmount()
  })

  it('连接失败在步骤内给出原因，不切换步骤', async () => {
    installSetupRuntime({
      test_llm_connection: () => {
        throw new Error('连接被拒绝')
      },
    })
    const wrapper = await mountSetup()

    await fillAndTest(wrapper)

    expect(wrapper.text()).toContain('连接被拒绝')
    expect(wrapper.text()).toContain('第 1 / 3 步')
    wrapper.unmount()
  })

  it('提交中整页锁定并输出受限标记', async () => {
    installSetupRuntime({ run_setup: () => new Promise(() => {}) })
    const wrapper = await mountSetup()

    await fillAndTest(wrapper)
    await buttonByText(wrapper, '下一步')?.trigger('click')
    await flushPromises()
    await buttonByText(wrapper, '跳过，稍后配置')?.trigger('click')
    await flushPromises()
    await buttonByText(wrapper, '保存并进入 Ramaria')?.trigger('click')
    await flushPromises()

    const root = wrapper.find('[data-view-state]')
    expect(root.attributes('data-view-state')).toBe('default')
    expect(root.attributes('data-restricted')).toBe('locked')
    expect(root.attributes('aria-busy')).toBe('true')
    wrapper.unmount()
  })
})
