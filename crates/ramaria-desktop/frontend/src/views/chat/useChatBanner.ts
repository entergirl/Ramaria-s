/**
 * src/views/chat/useChatBanner.ts — 对话视图横幅派生
 *
 * 职责:
 * - 按「只读 > 索引 > 摘要失败 > 隐私 > 模型不可用」的优先级选出唯一横幅
 * - 派生视图的降级标记（`data-degraded`）供状态断言与验收
 *
 * 设计特点:
 * - 文案一律取自 `stateCopy`，视图不写死状态说明
 * - 索引横幅是降级提示而非错误：构建中仍可继续对话
 * - 社交只读优先于其它提示，索引信息此时降为设置卡内标记
 */
import { computed } from 'vue'
import { useRouter } from 'vue-router'

import { stateCopy } from '../../core/stateCopy'
import { useAppStore } from '../../stores/app'
import { useChatStore } from '../../stores/chat'
import { useSessionStore } from '../../stores/session'

/** 横幅种类（决定图标与动作分支） */
export type ChatBannerKind =
  | 'readonly'
  | 'import'
  | 'index-idle'
  | 'index-building'
  | 'index-failed'
  | 'index-partial'
  | 'l1'
  | 'privacy'
  | 'llm'

/** 横幅展示数据 */
export interface ChatBannerView {
  kind: ChatBannerKind
  title: string
  description: string
  action?: string
  percent?: number
}

/** 横幅动作的外部依赖 */
export interface ChatBannerActions {
  openPrivacy: () => void
  retryL1: () => void
  startNewSession: () => void
}

export function useChatBanner(actions: ChatBannerActions) {
  const router = useRouter()
  const app = useAppStore()
  const session = useSessionStore()
  const chat = useChatStore()

  /** 对话模型不可用（应用降级且原因为模型侧） */
  const llmUnavailable = computed(
    () =>
      app.appState === 'degraded' &&
      (app.degradedReason === 'llm_unavailable' || app.degradedReason === 'both_unavailable'),
  )

  const banner = computed<ChatBannerView | null>(() => {
    if (session.readonly) {
      if (session.imported) {
        return {
          kind: 'import',
          title: '导入消息',
          description: '历史导入内容只读，可查看与复制正文。',
          action: '开始新对话',
        }
      }
      const copy = stateCopy('chat.restricted.readonly')
      return {
        kind: 'readonly',
        title: copy.title,
        description: copy.description,
        action: copy.action,
      }
    }

    const index = app.indexStatus
    if (index) {
      if (index.status === 'building') {
        const copy = stateCopy('chat.degraded.index.building')
        return {
          kind: 'index-building',
          title: `索引构建中 ${index.percent}%`,
          description: copy.description,
          action: copy.action,
          percent: index.percent,
        }
      }
      if (index.status === 'idle') {
        const copy = stateCopy('chat.degraded.index.idle')
        return { kind: 'index-idle', title: copy.title, description: copy.description, action: copy.action }
      }
      if (index.status === 'degraded') {
        const copy = stateCopy('chat.degraded.index.failed')
        return {
          kind: 'index-failed',
          title: copy.title,
          description: copy.description,
          action: copy.action,
        }
      }
      if (index.failed > 0) {
        const copy = stateCopy('chat.degraded.index.partial')
        return {
          kind: 'index-partial',
          title: copy.title,
          description: copy.description,
          action: copy.action,
        }
      }
    }

    if (chat.l1IssueSessionId && chat.l1IssueSessionId === session.activeId) {
      return {
        kind: 'l1',
        title: '摘要未生成',
        description: '对话已保存；摘要可在模型可用后重试。',
        action: '重试',
      }
    }

    if (chat.privacyNeedsConfirm) {
      const copy = stateCopy('chat.degraded.privacy')
      return { kind: 'privacy', title: copy.title, description: copy.description, action: copy.action }
    }

    if (llmUnavailable.value) {
      const copy = stateCopy('chat.degraded.llm.unavailable')
      return { kind: 'llm', title: copy.title, description: copy.description, action: copy.action }
    }

    return null
  })

  /** 视图降级标记：空格分隔的多值 */
  const degradedAttr = computed(() => {
    const parts: string[] = []
    if (app.indexNotReady || app.indexPartial) {
      parts.push('index')
    }
    if (llmUnavailable.value) {
      parts.push('llm')
    }
    if (chat.privacyNeedsConfirm) {
      parts.push('privacy')
    }
    return parts.length > 0 ? parts.join(' ') : undefined
  })

  /** 执行当前横幅的动作 */
  function runAction(): void {
    const current = banner.value
    if (!current?.action) {
      return
    }
    switch (current.kind) {
      case 'readonly':
      case 'import':
        actions.startNewSession()
        break
      case 'index-idle':
      case 'index-failed':
      case 'index-partial':
        void app.rebuildIndex()
        break
      case 'index-building':
        void router.push({ name: 'settings' })
        break
      case 'l1':
        actions.retryL1()
        break
      case 'privacy':
        actions.openPrivacy()
        break
      case 'llm':
        void router.push({ name: 'settings' })
        break
      default:
        break
    }
  }

  return { banner, degradedAttr, runAction }
}
