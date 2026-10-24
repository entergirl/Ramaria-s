/**
 * src/core/setupWizard.ts — 首次配置的纯逻辑
 *
 * 职责:
 * - 承载配置向导的表单默认值、字段校验、请求收集、摘要行与视觉能力配置补丁
 *
 * 设计特点:
 * - 全部为纯函数，不依赖 Vue / Pinia / Tauri，视图与测试可直接 import
 * - 校验文案按字段返回，视图只负责把消息挂到对应控件上
 * - 配置补丁只改动 `[vision].model_supports_vision`，其余配置键原样保留
 */

/** 对话模型运行方式 */
export type SetupMode = 'local' | 'online'

/** 向导步骤标签（三步：确认与完成同页，保存后切换为完成态） */
export const SETUP_STEPS = ['对话模型', '嵌入模型', '确认并进入'] as const

/** 本地推理服务默认地址（LM Studio 默认端口） */
export const DEFAULT_LOCAL_URL = 'http://127.0.0.1:1234/v1'

/** 线上 API 默认地址 */
export const DEFAULT_API_URL = 'https://api.deepseek.com/v1'

/**
 * 首次配置完成标记（`settings` 表内部键）。
 *
 * 说明:
 * - 后端的状态判定只看「后端配置存在 + 模型已选」，测试连接也会写入后端配置，
 *   因此需要一条独立标记区分「点过测试」与「走完向导」
 * - 标记的落点在服务层（M4-6）；在此之前由向导在保存成功后写入
 */
export const SETUP_COMPLETED_KEY = 'setup_completed_at'

/** 向导表单取值 */
export interface SetupFields {
  mode: SetupMode
  localUrl: string
  localModel: string
  apiUrl: string
  apiKey: string
  apiModel: string
  visionSupported: boolean
  embeddingPath: string
}

/** 表单字段级错误（空对象表示全部通过） */
export interface SetupFieldErrors {
  localUrl?: string
  localModel?: string
  apiUrl?: string
  apiKey?: string
  apiModel?: string
  embeddingPath?: string
}

/** 首次配置请求（与 `run_setup` 参数同名） */
export interface SetupRequestInput {
  provider: string
  modelId: string
  baseUrl: string
  apiKey: string | null
}

/** 确认步骤的摘要行 */
export interface SummaryRow {
  label: string
  value: string
  /** 需要强调的风险措辞（如降级） */
  warning?: boolean
  /** 以等宽字体呈现（地址、路径一类字面量） */
  mono?: boolean
  /** 该行由哪一步修改（在行尾给出「修改」） */
  target?: 1 | 2
}

/** 向导表单初始值 */
export function initialFields(): SetupFields {
  return {
    mode: 'local',
    localUrl: DEFAULT_LOCAL_URL,
    localModel: '',
    apiUrl: DEFAULT_API_URL,
    apiKey: '',
    apiModel: '',
    visionSupported: false,
    embeddingPath: '',
  }
}

/** 运行方式对应的 provider 标识（Rust 侧不区分大小写） */
export function providerOf(mode: SetupMode): string {
  return mode === 'local' ? 'LmStudio' : 'DeepSeek'
}

/** 是否形如 http(s) 地址 */
export function isHttpUrl(value: string): boolean {
  return /^https?:\/\/\S+$/i.test(value.trim())
}

/** 模型路径归一：去首尾空白，反斜杠统一为正斜杠 */
export function normalizeModelPath(raw: string): string {
  return raw.trim().replace(/\\/g, '/')
}

/** 是否记录到表单错误对象 */
export function hasFieldErrors(errors: SetupFieldErrors): boolean {
  return Object.values(errors).some((message) => !!message)
}

/** 校验对话模型步骤（按当前运行方式） */
export function validateLlmFields(fields: SetupFields): SetupFieldErrors {
  const errors: SetupFieldErrors = {}

  if (fields.mode === 'local') {
    if (!fields.localUrl.trim()) {
      errors.localUrl = '请填写推理服务地址'
    } else if (!isHttpUrl(fields.localUrl)) {
      errors.localUrl = '服务地址需以 http:// 或 https:// 开头'
    }
    if (!fields.localModel.trim()) {
      errors.localModel = '请填写模型名称'
    }
    return errors
  }

  if (!fields.apiUrl.trim()) {
    errors.apiUrl = '请填写 API 地址'
  } else if (!isHttpUrl(fields.apiUrl)) {
    errors.apiUrl = 'API 地址需以 http:// 或 https:// 开头'
  }
  if (!fields.apiKey.trim()) {
    errors.apiKey = '请填写 API Key'
  }
  if (!fields.apiModel.trim()) {
    errors.apiModel = '请填写模型名称'
  }
  return errors
}

/** 校验嵌入模型路径；返回空串表示通过 */
export function validateEmbeddingPath(raw: string): string {
  return raw.trim() ? '' : '请填写模型文件夹路径'
}

/** 收集首次配置请求 */
export function collectSetupRequest(fields: SetupFields): SetupRequestInput {
  if (fields.mode === 'local') {
    return {
      provider: providerOf('local'),
      modelId: fields.localModel.trim(),
      baseUrl: fields.localUrl.trim(),
      apiKey: null,
    }
  }
  return {
    provider: providerOf('online'),
    modelId: fields.apiModel.trim(),
    baseUrl: fields.apiUrl.trim(),
    apiKey: fields.apiKey.trim(),
  }
}

/** API Key 摘要取值：只显示掩码或未填写，不回显任何片段 */
export function maskedApiKey(apiKey: string): string {
  return apiKey.trim() ? '****' : '（未填写）'
}

/** 生成确认步骤的摘要行 */
export function summaryRows(input: {
  fields: SetupFields
  embeddingReady: boolean
  embeddingSkipped: boolean
}): SummaryRow[] {
  const { fields, embeddingReady, embeddingSkipped } = input
  const rows: SummaryRow[] = [
    {
      label: '对话模式',
      value: fields.mode === 'local' ? '本地部署' : '线上 API',
      target: 1,
    },
    {
      label: fields.mode === 'local' ? '推理服务地址' : 'API 地址',
      value: (fields.mode === 'local' ? fields.localUrl : fields.apiUrl).trim() || '-',
      mono: true,
    },
    {
      label: '模型名称',
      value: (fields.mode === 'local' ? fields.localModel : fields.apiModel).trim() || '-',
    },
    {
      label: '图片理解',
      value: fields.visionSupported ? '已声明模型支持' : '未声明（可在设置中开启）',
    },
  ]

  if (fields.mode === 'online') {
    rows.push({ label: 'API Key', value: maskedApiKey(fields.apiKey) })
  }

  if (embeddingReady) {
    rows.push({
      label: '嵌入模型',
      value: `已配置（${fields.embeddingPath.trim()}）`,
      mono: true,
      target: 2,
    })
  } else {
    rows.push({
      label: '嵌入模型',
      value: embeddingSkipped ? '已跳过（降级：仅关键词与图谱）' : '未配置（降级：仅关键词与图谱）',
      warning: true,
      target: 2,
    })
  }

  return rows
}

/** 判断值是否为普通对象 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** 读取配置中的图片理解声明（缺失按关闭处理） */
export function readVisionSupport(config: unknown): boolean {
  if (!isRecord(config) || !isRecord(config.vision)) {
    return false
  }
  return config.vision.model_supports_vision === true
}

/** 解析配置 JSON 文本；失败返回 `null` */
export function parseConfigJson(raw: string): unknown {
  try {
    return JSON.parse(raw) as unknown
  } catch {
    return null
  }
}

/** 是否已走完首次配置（`settings` 表中的完成标记非空） */
export function isSetupCompleted(settings: { key: string; value: string }[]): boolean {
  return settings.some(
    (item) => item.key === SETUP_COMPLETED_KEY && item.value.trim() !== '',
  )
}
