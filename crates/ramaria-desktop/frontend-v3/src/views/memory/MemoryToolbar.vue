<script setup lang="ts">
/**
 * src/views/memory/MemoryToolbar.vue — 记忆页工具栏
 *
 * 职责:
 * - 展示当前人格头像、名称与分类，作为人格卡的展开入口
 * - 承载「深度处理导入的消息」入口（仅导入人格可用）
 *
 * 设计特点:
 * - 头像独立成按钮，昵称与分类只作文本展示，不承担点击
 * - 展开态经 aria-expanded 透出，与人格卡 aria-controls 对应
 * - 工具栏始终全宽，不随人格卡收缩
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaSummary } from '../../core/api/dto'
import { personaCategory, personaInitial } from '../../core/memory/format'

const props = defineProps<{
  persona: PersonaSummary | null
  open: boolean
  canDeepProcess: boolean
  processing: boolean
}>()

defineEmits<{
  toggle: []
  'deep-process': []
}>()

const name = computed(() => props.persona?.name ?? '未选择人格')
const initial = computed(() => personaInitial(props.persona?.name))
const category = computed(() =>
  props.persona ? personaCategory(props.persona.kind, props.persona.source) : null,
)
</script>

<template>
  <section class="toolbar">
    <div class="persona-filter">
      <button
        id="memory-persona-trigger"
        type="button"
        class="avatar"
        :aria-expanded="open ? 'true' : 'false'"
        aria-haspopup="dialog"
        aria-controls="memory-personas"
        :aria-label="open ? '收起人格列表' : '打开人格列表'"
        @click="$emit('toggle')"
      >
        <span class="avatar-ring" aria-hidden="true" />
        <span class="avatar-text">{{ initial }}</span>
      </button>
      <div class="persona-filter-info">
        <div class="persona-filter-name">{{ name }}</div>
        <BaseTag v-if="category" :tone="category.tone" class="persona-filter-cat">
          {{ category.label }}
        </BaseTag>
      </div>
    </div>

    <span class="spacer" />

    <BaseButton
      v-if="canDeepProcess"
      variant="ghost"
      size="sm"
      :loading="processing"
      @click="$emit('deep-process')"
    >
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 3v3M12 18v3M4.2 7.5l2.6 1.5M17.2 15l2.6 1.5M4.2 16.5l2.6-1.5M17.2 9l2.6-1.5" />
        <circle cx="12" cy="12" r="3.4" />
      </svg>
      深度处理导入的消息
    </BaseButton>
  </section>
</template>

<style scoped>
.toolbar {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-14);
  padding: var(--space-14) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.persona-filter {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  min-width: 0;
}

.avatar {
  position: relative;
  display: grid;
  flex: none;
  place-items: center;
  width: 38px;
  height: 38px;
  padding: 0;
  border: none;
  background: transparent;
  color: var(--brand-text);
  font-size: var(--fs-body-lg);
  font-weight: var(--fw-bold);
  line-height: 1;
  cursor: pointer;
}

.avatar-ring {
  position: absolute;
  inset: 0;
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.avatar-text {
  position: relative;
  z-index: 1;
  line-height: 1;
  transform: translate(0.04em, 0.02em);
}

.avatar:hover .avatar-ring {
  box-shadow: var(--shadow-sm);
}

.avatar:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.avatar[aria-expanded='true'] .avatar-ring {
  border-color: var(--brand);
  box-shadow: 0 0 0 3px var(--brand-surface);
}

.persona-filter-info {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.persona-filter-name {
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
  line-height: 1.2;
}

.persona-filter-cat {
  align-self: flex-start;
}

.spacer {
  flex: 1;
}

.icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

@media (max-width: 720px) {
  .toolbar {
    flex-wrap: wrap;
  }
}
</style>
