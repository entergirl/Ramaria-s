/**
 * tests/transport.test.ts — 传输层用例
 *
 * 覆盖:
 * - 运行时缺失时报错并保留命令名
 * - 命令名与参数原样透传
 * - 宿主字符串错误被包装为传输层错误且保留原文
 */

import { afterEach, describe, expect, it, vi } from 'vitest'

import { invoke, setTauriRuntime, TransportError } from '../src/core/transport'

afterEach(() => {
  setTauriRuntime(null)
})

describe('invoke', () => {
  it('运行时缺失时抛传输层错误并保留命令名', async () => {
    await expect(invoke('get_version')).rejects.toThrow(TransportError)
    await expect(invoke('get_version')).rejects.toMatchObject({ command: 'get_version' })
  })

  it('原样透传命令名与参数并返回结果', async () => {
    const call = vi.fn().mockResolvedValue('3.0.0')
    setTauriRuntime({ core: { invoke: call } })

    await expect(invoke<string>('get_version', { verbose: true })).resolves.toBe('3.0.0')
    expect(call).toHaveBeenCalledWith('get_version', { verbose: true })
  })

  it('宿主字符串错误包装为传输层错误并保留原文', async () => {
    setTauriRuntime({ core: { invoke: vi.fn().mockRejectedValue('图片序号越界') } })

    await expect(invoke('read_attachment_image')).rejects.toThrow('图片序号越界')
    await expect(invoke('read_attachment_image')).rejects.toBeInstanceOf(TransportError)
  })
})
