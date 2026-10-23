/**
 * src/core/transport.ts — 桌面运行时传输层
 *
 * 职责:
 * - 唯一直接访问 Tauri 注入全局对象的模块；上层经 `invoke` 收发命令
 * - 把底层异常统一包装为 `TransportError`，保留命令名与原始消息
 *
 * 设计特点:
 * - 运行时对象可注入，单元测试不依赖真实 WebView 宿主
 * - 严格 CSP 下不加载远程脚本、不做运行时求值，只读取宿主注入的全局对象
 * - 命令名与参数原样透传，业务校验属上层，传输层不解释返回结构
 */

/** Tauri 运行时注入对象的最小面（命令调用与事件订阅） */
export interface TauriRuntime {
  core: {
    invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>
  }
  event?: {
    listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void>
    emit: (event: string, payload?: unknown) => Promise<void>
  }
}

declare global {
  interface Window {
    __TAURI__?: TauriRuntime
  }
}

/** 传输层错误：附命令名，供上层按命令分流文案 */
export class TransportError extends Error {
  readonly command: string

  constructor(command: string, message: string) {
    super(message)
    this.name = 'TransportError'
    this.command = command
  }
}

let injected: TauriRuntime | null = null

/** 注入运行时（宿主桥接与测试使用；传 `null` 清除注入） */
export function setTauriRuntime(runtime: TauriRuntime | null): void {
  injected = runtime
}

/** 当前运行时；浏览器直开或注入缺失时为 `null` */
export function getTauriRuntime(): TauriRuntime | null {
  if (injected) {
    return injected
  }
  if (typeof window === 'undefined') {
    return null
  }
  return window.__TAURI__ ?? null
}

/** 桌面运行时是否可用 */
export function isTauriAvailable(): boolean {
  return getTauriRuntime() !== null
}

/** 调用命令；失败统一抛 `TransportError` */
export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  const runtime = getTauriRuntime()
  if (!runtime) {
    throw new TransportError(command, '桌面运行时不可用')
  }
  try {
    return (await runtime.core.invoke(command, args)) as T
  } catch (error) {
    throw new TransportError(command, describeError(error))
  }
}

/** 底层异常转文案：命令报错为字符串，其余按 Error / 未知处理 */
function describeError(error: unknown): string {
  if (typeof error === 'string') {
    return error
  }
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}
