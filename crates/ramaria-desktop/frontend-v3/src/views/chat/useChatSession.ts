/**
 * src/views/chat/useChatSession.ts — 对话视图的会话编排
 *
 * 职责:
 * - 启动装载（人格 / 会话列表 / 隐私状态）与首屏会话选择
 * - 切换、新建、发送、保存对话、撤回、发图与 L1 重试的动作编排
 * - 承接跨视图跳转参数与通知点击定位
 *
 * 设计特点:
 * - 动作只做编排，消息与状态由 store 持有，视图不直连命令
 * - 切换会话在生成期间被拒绝，避免流式文本落到错误会话
 * - 新建会话不预先落库：首次发送时才创建，避免堆积空会话
 */
import { reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import { chatApi, systemApi } from '../../core/api'
import type { SessionMessage } from '../../core/api/dto'
import { parseL1Receipt, parseSaveReceipt } from '../../core/chat/receipt'
import { errorMessage } from '../../core/errors'
import { pushToast } from '../../core/toast'
import { useChatStore } from '../../stores/chat'
import { useSessionStore } from '../../stores/session'

/** 发图可选扩展名 */
const IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp']

export function useChatSession() {
  const router = useRouter()
  const route = useRoute()
  const session = useSessionStore()
  const chat = useChatStore()

  const sessionsOpen = ref(false)
  const opening = ref(false)
  const saving = ref(false)
  const sendingImage = ref(false)
  const recalling = ref(false)
  const recallTarget = ref<SessionMessage | null>(null)
  const focusMessageId = ref<string | null>(null)
  const privacyOpen = ref(false)
  const privacyDraft = reactive({ accepted: false, persistent: true })
  const initialized = ref(false)

  /** 新会话 / 发送使用的目标人格 */
  function targetPersona(): string | null {
    return session.activePersona?.uid ?? session.defaultPersona?.uid ?? null
  }

  /**
   * 确保存在可写入的会话。
   *
   * 返回:
   * - 会话标识；创建失败返回 null（错误已提示）
   */
  async function ensureSession(): Promise<string | null> {
    if (session.activeId && !session.closed) {
      return session.activeId
    }
    const summary = await session.create(targetPersona())
    if (!summary) {
      pushToast({ tone: 'danger', message: session.error || '新建会话失败' })
      return null
    }
    chat.clear()
    return summary.id
  }

  /**
   * 打开指定会话。
   *
   * 参数:
   * - `sessionId`: 目标会话
   * - `closeCard`: 是否同时收起会话卡片
   */
  async function openSession(sessionId: string, closeCard = true): Promise<boolean> {
    if (chat.streaming) {
      pushToast({ tone: 'warn', message: '正在生成回复，稍后再切换会话' })
      return false
    }
    if (opening.value) {
      return false
    }
    opening.value = true
    try {
      const detail = await session.open(sessionId)
      if (!detail) {
        pushToast({ tone: 'danger', message: session.error || '会话加载失败' })
        return false
      }
      chat.loadMessages(detail.messages)
      if (closeCard) {
        sessionsOpen.value = false
      }
      return true
    } finally {
      opening.value = false
    }
  }

  /** 新建空白会话：清空当前会话，不落库 */
  function startNewSession(): void {
    if (chat.streaming) {
      pushToast({ tone: 'warn', message: '正在生成回复，稍后再新建会话' })
      return
    }
    session.clearActive()
    chat.clear()
    sessionsOpen.value = false
  }

  /**
   * 发送文本。
   *
   * 参数:
   * - `text`: 用户输入
   */
  async function send(text: string): Promise<void> {
    const trimmed = text.trim()
    if (!trimmed || session.readonly) {
      return
    }
    if (!chat.canCompose) {
      return
    }
    const sessionId = await ensureSession()
    if (!sessionId) {
      return
    }
    const ok = await chat.send(trimmed, targetPersona(), sessionId)
    if (!ok) {
      pushToast({ tone: 'danger', message: chat.error || '发送失败' })
    }
  }

  /** 重试上一次失败的用户输入 */
  async function retrySend(): Promise<void> {
    const text = chat.takeFailedText()
    if (text) {
      await send(text)
    }
  }

  /** 保存对话：关闭活跃会话并触发 L1 摘要 */
  async function saveConversation(): Promise<void> {
    if (saving.value || chat.streaming) {
      return
    }
    saving.value = true
    try {
      const raw = await chatApi.save(targetPersona())
      const receipt = parseSaveReceipt(raw)
      if (!receipt) {
        pushToast({ tone: 'warn', message: '保存结果未知，请确认会话状态' })
        return
      }
      if (receipt.status === 'no_active_session') {
        pushToast({ tone: 'info', message: '当前没有进行中的对话' })
        return
      }

      const sessionId = receipt.session_id ?? session.activeId
      if (sessionId) {
        await session.open(sessionId)
        await session.loadList()
      }
      if (receipt.l1_generated) {
        chat.markL1Issue(null)
        pushToast({ tone: 'success', message: '对话已保存，摘要已生成' })
      } else {
        chat.markL1Issue(sessionId ?? null)
        pushToast({ tone: 'warn', message: '对话已保存，摘要未生成，可重试' })
      }
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '保存对话失败') })
    } finally {
      saving.value = false
    }
  }

  /** 重试 L1 摘要 */
  async function retryL1(): Promise<void> {
    const sessionId = chat.l1IssueSessionId ?? session.activeId
    if (!sessionId) {
      return
    }
    try {
      const raw = await chatApi.generateL1(sessionId, targetPersona())
      const receipt = parseL1Receipt(raw)
      if (receipt?.l1_generated) {
        chat.markL1Issue(null)
        pushToast({ tone: 'success', message: '摘要已生成' })
        return
      }
      pushToast({ tone: 'warn', message: '摘要仍未生成，请确认对话模型可用' })
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '摘要生成失败') })
    }
  }

  /** 选择图片并发送到当前会话 */
  async function attachImage(): Promise<void> {
    if (session.readonly || sendingImage.value || !chat.canCompose) {
      return
    }
    sendingImage.value = true
    try {
      const initialDir = await systemApi.getImageSourceDir().catch(() => null)
      const picked = await systemApi.openFileDialog({
        initialDir,
        title: '选择要发送的图片',
        filterName: '图片',
        extensions: IMAGE_EXTENSIONS,
      })
      if (!picked) {
        return
      }

      const sessionId = await ensureSession()
      if (!sessionId) {
        return
      }
      const result = await chatApi.sendLocalImage(sessionId, picked, null)
      chat.appendImage(result)
      void chat.reconcile(sessionId)
    } catch (failure) {
      const message = errorMessage(failure, '发送图片失败')
      const outsideDir = message.includes('目录')
      pushToast({
        tone: 'danger',
        message,
        action: outsideDir
          ? { label: '去设置', run: () => void router.push({ name: 'settings' }) }
          : { label: '重试', run: () => void attachImage() },
      })
    } finally {
      sendingImage.value = false
    }
  }

  /** 请求撤回某条消息（先确认） */
  function requestRecall(message: SessionMessage): void {
    recallTarget.value = message
  }

  /** 确认撤回 */
  async function confirmRecall(): Promise<void> {
    const target = recallTarget.value
    if (!target || recalling.value) {
      return
    }
    recalling.value = true
    try {
      const failure = await chat.recall(target.id)
      if (failure) {
        pushToast({ tone: 'danger', message: failure })
      } else {
        pushToast({ tone: 'success', message: '消息已撤回' })
      }
    } finally {
      recalling.value = false
      recallTarget.value = null
    }
  }

  /** 打开隐私确认 */
  function openPrivacy(): void {
    privacyDraft.accepted = false
    privacyDraft.persistent = true
    privacyOpen.value = true
  }

  /** 提交隐私确认 */
  async function confirmPrivacy(): Promise<void> {
    if (!privacyDraft.accepted) {
      return
    }
    const failure = await chat.confirmPrivacy(privacyDraft.persistent)
    privacyOpen.value = false
    if (failure) {
      pushToast({ tone: 'danger', message: failure })
      return
    }
    pushToast({ tone: 'success', message: '隐私范围已确认' })
  }

  /** 启动装载与首屏会话选择 */
  async function init(): Promise<void> {
    if (initialized.value) {
      return
    }
    initialized.value = true

    await Promise.all([session.loadPersonas(), session.loadList(), chat.loadPrivacy()])

    const querySession = Array.isArray(route.query.sessionId)
      ? route.query.sessionId[0]
      : route.query.sessionId
    if (querySession) {
      const ok = await openSession(String(querySession), false)
      if (!ok) {
        pushToast({ tone: 'warn', message: '目标会话不存在或已删除' })
      }
      void router.replace({ name: 'chat' })
      return
    }

    const latest = session.items.find(
      (item) => (item.channel || 'local') === 'local' && item.ended_at === null,
    )
    if (latest) {
      await openSession(latest.id, false)
    }
  }

  // 通知点击定位：切到目标会话并高亮该条消息
  watch(
    () => chat.activation,
    (value) => {
      if (!value) {
        return
      }
      const pending = chat.takeActivation()
      if (!pending) {
        return
      }
      void (async () => {
        if (session.activeId !== pending.sessionId) {
          const ok = await openSession(pending.sessionId, false)
          if (!ok) {
            return
          }
        }
        focusMessageId.value = pending.messageId
        window.setTimeout(() => {
          if (focusMessageId.value === pending.messageId) {
            focusMessageId.value = null
          }
        }, 2600)
      })()
    },
  )

  return {
    session,
    chat,
    sessionsOpen,
    saving,
    sendingImage,
    recalling,
    recallTarget,
    focusMessageId,
    privacyOpen,
    privacyDraft,
    init,
    openSession,
    startNewSession,
    send,
    retrySend,
    saveConversation,
    retryL1,
    attachImage,
    requestRecall,
    confirmRecall,
    openPrivacy,
    confirmPrivacy,
    targetPersona,
  }
}
