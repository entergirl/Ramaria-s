/**
 * src/core/useAsyncData.ts — 异步数据装载
 *
 * 职责:
 * - 统一「主请求」的加载 / 错误 / 数据三态，供视图派生六态
 *
 * 设计特点:
 * - 只表达主态，不承担降级与受限（叠加态由 store 派生）
 * - 重入保护：同一实例在途时重复调用直接复用上次 Promise
 * - 失败保留上一次成功数据，配合 `--opacity-dim` 表达刷新态旧内容
 */

import { ref, shallowRef, type Ref } from 'vue'

import { errorMessage } from './errors'

export interface AsyncData<T> {
  data: Ref<T | null>
  loading: Ref<boolean>
  error: Ref<string>
  load: () => Promise<void>
  reset: () => void
}

/**
 * 创建异步数据源。
 *
 * 参数:
 * - `loader`: 取数函数，抛错即进入错误态
 * - `immediate`: 是否立即装载（默认 true）
 */
export function useAsyncData<T>(loader: () => Promise<T>, immediate = true): AsyncData<T> {
  const data = shallowRef<T | null>(null)
  const loading = ref(false)
  const error = ref('')
  let pending: Promise<void> | null = null

  async function load(): Promise<void> {
    if (pending) {
      return pending
    }
    loading.value = true
    error.value = ''
    pending = (async () => {
      try {
        data.value = await loader()
      } catch (failure) {
        error.value = errorMessage(failure, '读取失败')
      } finally {
        loading.value = false
        pending = null
      }
    })()
    return pending
  }

  function reset(): void {
    data.value = null
    loading.value = false
    error.value = ''
    pending = null
  }

  if (immediate) {
    void load()
  }

  return { data, loading, error, load, reset }
}
