<script setup lang="ts">
/**
 * src/components/base/BaseModal.vue — 模态框
 *
 * 职责:
 * - 承载需要聚焦的确认、表单与详情内容
 *
 * 设计特点:
 * - Esc 与关闭键永不屏蔽；遮罩点击等同关闭
 * - 打开时焦点送入框内，Tab 在框内循环，关闭后焦点回到原处
 * - 危险形态只改变视觉与按钮语气，二次确认由调用方组织
 */

import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'

const props = withDefaults(
  defineProps<{
    open: boolean
    title?: string
    variant?: 'default' | 'danger'
    loading?: boolean
  }>(),
  {
    title: '',
    variant: 'default',
    loading: false,
  },
)

const emit = defineEmits<{
  close: []
}>()

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

const dialog = ref<HTMLElement | null>(null)
let lastFocused: HTMLElement | null = null

function focusables(): HTMLElement[] {
  const root = dialog.value
  if (!root) {
    return []
  }
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE))
}

function focusFirst(): void {
  const items = focusables()
  const target = items[0] ?? dialog.value
  target?.focus({ preventScroll: true })
}

function close(): void {
  emit('close')
}

function onKeydown(event: KeyboardEvent): void {
  if (!props.open) {
    return
  }
  if (event.key === 'Escape') {
    event.stopPropagation()
    close()
    return
  }
  if (event.key !== 'Tab') {
    return
  }
  const items = focusables()
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
      lastFocused = document.activeElement as HTMLElement | null
      void nextTick(focusFirst)
      return
    }
    lastFocused?.focus({ preventScroll: true })
    lastFocused = null
  },
)

onMounted(() => {
  document.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKeydown)
})
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="modal-veil" @click.self="close">
        <section
          ref="dialog"
          class="modal-card"
          role="dialog"
          aria-modal="true"
          :aria-label="title || undefined"
          :aria-busy="loading ? 'true' : undefined"
          :data-variant="variant"
          tabindex="-1"
        >
          <header class="modal-head">
            <h2 v-if="title" class="modal-title">{{ title }}</h2>
            <slot name="head" />
            <button type="button" class="modal-close" aria-label="关闭" @click="close">
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path d="M2.5 2.5l7 7M9.5 2.5l-7 7" />
              </svg>
            </button>
          </header>
          <div class="modal-body" :data-loading="loading ? '' : undefined">
            <slot />
          </div>
          <footer v-if="$slots.foot" class="modal-foot">
            <slot name="foot" />
          </footer>
        </section>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.modal-veil {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  display: grid;
  place-items: center;
  padding: var(--space-24);
  background: var(--overlay-scrim);
}

.modal-card {
  display: flex;
  flex-direction: column;
  width: min(600px, 100%);
  max-height: min(84vh, 760px);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-modal);
  box-shadow: var(--shadow-lg);
  overflow: hidden;
}

.modal-card:focus-visible {
  outline: none;
}

.modal-head {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-16) var(--space-20) var(--space-12);
  border-bottom: 1px solid var(--border);
}

.modal-title {
  flex: 1;
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h3);
}

.modal-card[data-variant='danger'] .modal-title {
  color: var(--danger);
}

.modal-close {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  padding: 0;
  border: none;
  border-radius: var(--radius-btn-sm);
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
}

.modal-close svg {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.4;
  stroke-linecap: round;
}

.modal-close:hover {
  background: var(--bg-subtle);
  color: var(--text-1);
}

.modal-close:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.modal-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: var(--space-16) var(--space-20);
  color: var(--text-1);
  font-size: var(--fs-body);
}

.modal-body[data-loading] {
  opacity: var(--opacity-dim);
}

.modal-foot {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-12) var(--space-20) var(--space-16);
  border-top: 1px solid var(--border);
}

.modal-enter-active,
.modal-leave-active {
  transition: opacity var(--dur-base) var(--ease-out);
}

.modal-enter-active .modal-card,
.modal-leave-active .modal-card {
  transition: transform var(--dur-base) var(--ease-pop);
}

.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}

.modal-enter-from .modal-card {
  transform: translateY(8px) scale(0.98);
}

.modal-leave-to .modal-card {
  transform: translateY(4px) scale(0.99);
}

@media (prefers-reduced-motion: reduce) {
  .modal-enter-active .modal-card,
  .modal-leave-active .modal-card {
    transition: none;
  }
}
</style>
