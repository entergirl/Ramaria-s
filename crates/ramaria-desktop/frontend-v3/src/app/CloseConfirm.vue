<script setup lang="ts">
/**
 * src/app/CloseConfirm.vue — 关闭确认弹窗
 *
 * 职责:
 * - 用户点击窗口关闭键时承接后端 `close-requested`，让用户在「最小化到托盘 / 退出」中二选一
 * - 提交选择后调用 `confirm_close_action`
 *
 * 设计特点:
 * - 不可关闭：无关闭键、Esc 与遮罩点击均不生效（后端 5 秒无响应会兜底隐藏到托盘，误关代价高）
 * - 焦点落在「最小化到托盘」，Tab 在两个动作间循环，不落到底层页面
 * - 提交中锁定两个动作，避免重复调用
 */

import { nextTick, onBeforeUnmount, ref, watch } from 'vue'

import BaseButton from '../components/base/BaseButton.vue'

const props = withDefaults(
  defineProps<{
    open: boolean
    busy?: boolean
  }>(),
  { busy: false },
)

const emit = defineEmits<{
  confirm: [action: 'minimize' | 'exit']
}>()

const dialog = ref<HTMLElement | null>(null)

function actions(): HTMLButtonElement[] {
  const root = dialog.value
  if (!root) {
    return []
  }
  return Array.from(root.querySelectorAll<HTMLButtonElement>('button:not([disabled])'))
}

function focusFirst(): void {
  actions()[0]?.focus({ preventScroll: true })
}

function onKeydown(event: KeyboardEvent): void {
  if (!props.open) {
    return
  }
  // 系统级确认：Esc 只吞掉，不关闭
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    return
  }
  if (event.key !== 'Tab') {
    return
  }
  const items = actions()
  if (items.length === 0) {
    event.preventDefault()
    dialog.value?.focus({ preventScroll: true })
    return
  }
  const first = items[0]
  const last = items[items.length - 1]
  const active = document.activeElement
  if (event.shiftKey && (active === first || active === dialog.value)) {
    event.preventDefault()
    last?.focus()
    return
  }
  if (!event.shiftKey && active === last) {
    event.preventDefault()
    first?.focus()
  }
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      void nextTick(focusFirst)
      document.addEventListener('keydown', onKeydown, true)
      return
    }
    document.removeEventListener('keydown', onKeydown, true)
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown, true)
})
</script>

<template>
  <Teleport to="body">
    <Transition name="confirm">
      <div v-if="open" class="confirm-veil">
        <section
          ref="dialog"
          class="confirm-card"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="close-confirm-title"
          :aria-busy="busy ? 'true' : undefined"
          tabindex="-1"
        >
          <h2 id="close-confirm-title" class="confirm-title">关闭 Ramaria</h2>
          <p class="confirm-lead">请选择关闭方式：</p>
          <ul class="confirm-list">
            <li>
              <strong>最小化到托盘</strong>：窗口隐藏，应用在后台继续运行，可通过系统托盘图标恢复。
            </li>
            <li>
              <strong>退出 Ramaria</strong>：完全关闭应用，停止所有后台任务。
            </li>
          </ul>
          <div class="confirm-foot">
            <BaseButton variant="secondary" :disabled="busy" @click="emit('confirm', 'minimize')">
              最小化到托盘
            </BaseButton>
            <BaseButton variant="danger" :disabled="busy" @click="emit('confirm', 'exit')">
              退出 Ramaria
            </BaseButton>
          </div>
        </section>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.confirm-veil {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  display: grid;
  place-items: center;
  padding: var(--space-24);
  background: var(--overlay-scrim);
}

.confirm-card {
  display: flex;
  flex-direction: column;
  width: min(420px, 100%);
  padding: var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-modal);
  box-shadow: var(--shadow-lg);
}

.confirm-card:focus-visible {
  outline: none;
}

.confirm-title {
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h3);
}

.confirm-lead {
  margin-top: var(--space-10);
  color: var(--text-2);
  font-size: var(--fs-body);
}

.confirm-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  margin: var(--space-8) 0 0;
  padding-left: var(--space-16);
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
  list-style: disc;
}

.confirm-list strong {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.confirm-foot {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-8);
  margin-top: var(--space-20);
}

.confirm-enter-active,
.confirm-leave-active {
  transition: opacity var(--dur-base) var(--ease-out);
}

.confirm-enter-active .confirm-card,
.confirm-leave-active .confirm-card {
  transition: transform var(--dur-base) var(--ease-pop);
}

.confirm-enter-from,
.confirm-leave-to {
  opacity: 0;
}

.confirm-enter-from .confirm-card {
  transform: translateY(8px) scale(0.98);
}

@media (prefers-reduced-motion: reduce) {
  .confirm-enter-active .confirm-card,
  .confirm-leave-active .confirm-card {
    transition: none;
  }
}
</style>
