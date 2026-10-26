/**
 * tests/setupWizard.test.ts — 首次配置纯逻辑守卫
 *
 * 覆盖:
 * - 表单默认值与字段校验（本地 / 线上两套字段）
 * - 请求收集、摘要行与 API Key 掩码
 * - 图片理解声明的读取与配置补丁（保留其余配置组）
 */

import { describe, expect, it } from 'vitest'

import {
  collectSetupRequest,
  DEFAULT_API_URL,
  DEFAULT_LOCAL_URL,
  initialFields,
  isHttpUrl,
  isSetupCompleted,
  maskedApiKey,
  normalizeModelPath,
  parseConfigJson,
  readVisionSupport,
  SETUP_COMPLETED_KEY,
  summaryRows,
  validateEmbeddingPath,
  validateLlmFields,
} from '../src/core/setupWizard'

describe('表单默认值', () => {
  it('默认本地部署并预填服务地址，图片理解关闭', () => {
    const fields = initialFields()
    expect(fields.mode).toBe('local')
    expect(fields.localUrl).toBe(DEFAULT_LOCAL_URL)
    expect(fields.apiUrl).toBe(DEFAULT_API_URL)
    expect(fields.visionSupported).toBe(false)
  })
})

describe('对话模型字段校验', () => {
  it('本地模式要求地址可解析且模型名非空', () => {
    const fields = { ...initialFields(), localUrl: 'localhost:1234', localModel: ' ' }
    const errors = validateLlmFields(fields)
    expect(errors.localUrl).toContain('http')
    expect(errors.localModel).toContain('模型名称')
  })

  it('本地模式字段齐全时无错误', () => {
    const fields = { ...initialFields(), localModel: 'qwen3-8b' }
    expect(validateLlmFields(fields)).toEqual({})
  })

  it('线上模式要求地址、密钥与模型名', () => {
    const fields = { ...initialFields(), mode: 'online' as const, apiUrl: '', apiKey: '', apiModel: '' }
    const errors = validateLlmFields(fields)
    expect(errors.apiUrl).toBeTruthy()
    expect(errors.apiKey).toBeTruthy()
    expect(errors.apiModel).toBeTruthy()
  })

  it('只接受 http(s) 地址', () => {
    expect(isHttpUrl('https://api.deepseek.com/v1')).toBe(true)
    expect(isHttpUrl('http://127.0.0.1:1234/v1')).toBe(true)
    expect(isHttpUrl('ws://127.0.0.1')).toBe(false)
  })
})

describe('嵌入模型路径', () => {
  it('反斜杠统一为正斜杠并去空白', () => {
    expect(normalizeModelPath('  D:\\models\\bge-small  ')).toBe('D:/models/bge-small')
  })

  it('空路径给出字段错误，非空通过', () => {
    expect(validateEmbeddingPath('   ')).toContain('路径')
    expect(validateEmbeddingPath('D:/models/bge')).toBe('')
  })
})

describe('请求收集', () => {
  it('本地模式不带密钥且 provider 为 LmStudio', () => {
    const fields = { ...initialFields(), localModel: ' qwen3-8b ', apiKey: 'ignored' }
    expect(collectSetupRequest(fields)).toEqual({
      provider: 'LmStudio',
      modelId: 'qwen3-8b',
      baseUrl: DEFAULT_LOCAL_URL,
      apiKey: null,
    })
  })

  it('线上模式取线上字段并保留密钥', () => {
    const fields = {
      ...initialFields(),
      mode: 'online' as const,
      apiUrl: ' https://api.deepseek.com/v1 ',
      apiKey: ' sk-test ',
      apiModel: ' deepseek-chat ',
    }
    expect(collectSetupRequest(fields)).toEqual({
      provider: 'DeepSeek',
      modelId: 'deepseek-chat',
      baseUrl: 'https://api.deepseek.com/v1',
      apiKey: 'sk-test',
    })
  })
})

describe('确认摘要', () => {
  it('本地已配置嵌入模型：不出现密钥行，嵌入行不告警', () => {
    const fields = { ...initialFields(), localModel: 'qwen3-8b', embeddingPath: 'D:/models/bge' }
    const rows = summaryRows({ fields, embeddingReady: true, embeddingSkipped: false })
    expect(rows.map((row) => row.label)).not.toContain('API Key')
    const embedding = rows.find((row) => row.label === '嵌入模型')
    expect(embedding?.value).toContain('D:/models/bge')
    expect(embedding?.warning).toBeUndefined()
  })

  it('线上跳过嵌入：密钥只显示掩码，嵌入行标降级', () => {
    const fields = {
      ...initialFields(),
      mode: 'online' as const,
      apiKey: 'sk-secret-value',
      apiModel: 'deepseek-chat',
    }
    const rows = summaryRows({ fields, embeddingReady: false, embeddingSkipped: true })
    expect(rows.find((row) => row.label === 'API Key')?.value).toBe('****')
    const embedding = rows.find((row) => row.label === '嵌入模型')
    expect(embedding?.warning).toBe(true)
    expect(embedding?.value).toContain('降级')
  })

  it('密钥掩码不回显任何片段', () => {
    expect(maskedApiKey('sk-secret')).toBe('****')
    expect(maskedApiKey('   ')).toBe('（未填写）')
  })

  it('地址与路径行用等宽芯片，对话模式与嵌入模型行可回到对应步骤', () => {
    const fields = { ...initialFields(), localModel: 'qwen3-8b' }
    const rows = summaryRows({ fields, embeddingReady: true, embeddingSkipped: false })
    expect(rows.find((row) => row.label === '对话模式')?.target).toBe(1)
    expect(rows.find((row) => row.label === '推理服务地址')?.mono).toBe(true)
    expect(rows.find((row) => row.label === '嵌入模型')?.target).toBe(2)
    expect(rows.find((row) => row.label === '模型名称')?.mono).toBeUndefined()
  })
})

describe('图片理解声明', () => {
  it('从配置读取声明，缺失与非对象按关闭处理', () => {
    expect(readVisionSupport({ vision: { model_supports_vision: true } })).toBe(true)
    expect(readVisionSupport({ vision: { model_supports_vision: false } })).toBe(false)
    expect(readVisionSupport({})).toBe(false)
    expect(readVisionSupport('bad')).toBe(false)
  })

  it('配置 JSON 解析失败返回 null', () => {
    expect(parseConfigJson('{"a":1}')).toEqual({ a: 1 })
    expect(parseConfigJson('not json')).toBeNull()
  })
})

describe('首次配置完成标记', () => {
  it('缺少标记或值为空时视为未完成', () => {
    expect(isSetupCompleted([])).toBe(false)
    expect(isSetupCompleted([{ key: 'image_source_dir', value: '/tmp' }])).toBe(false)
    expect(isSetupCompleted([{ key: SETUP_COMPLETED_KEY, value: '   ' }])).toBe(false)
  })

  it('标记非空时视为已完成', () => {
    expect(isSetupCompleted([{ key: SETUP_COMPLETED_KEY, value: '1781' }])).toBe(true)
  })
})
