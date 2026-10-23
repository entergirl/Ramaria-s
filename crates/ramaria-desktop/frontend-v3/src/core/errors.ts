/**
 * src/core/errors.ts — 错误归一
 *
 * 职责:
 * - 把传输层异常与未知异常收敛为统一结构，供视图与提示层消费
 *
 * 设计特点:
 * - 只做形状归一与文案提取，不在此处拼接面向用户的长句（文案在 stateCopy）
 * - 保留命令名，便于按命令分流处理与日志定位
 * - 不暴露堆栈与内部路径给界面，技术细节只进控制台
 */

import { TransportError } from './transport'

/** 全局错误形状 */
export interface AppError {
  /** 触发的命令名（非命令调用产生的错误为空串） */
  command: string
  /** 面向用户的短句，直接来自后端业务错误或兜底文案 */
  message: string
  /** 是否传输层产生的错误（运行时缺失 / IPC 失败） */
  transport: boolean
}

/** 把任意异常收敛为 `AppError` */
export function toAppError(error: unknown, fallback = '操作失败'): AppError {
  if (error instanceof TransportError) {
    return { command: error.command, message: error.message, transport: true }
  }
  if (error instanceof Error) {
    return { command: '', message: error.message || fallback, transport: false }
  }
  if (typeof error === 'string' && error.trim()) {
    return { command: '', message: error, transport: false }
  }
  return { command: '', message: fallback, transport: false }
}

/** 取可直接展示的一句话（失败时给兜底文案） */
export function errorMessage(error: unknown, fallback = '操作失败'): string {
  return toAppError(error, fallback).message
}
