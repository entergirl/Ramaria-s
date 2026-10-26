<script setup lang="ts">
/**
 * src/components/shell/PersonaFilterButton.vue — 人格筛选触发块
 *
 * 职责:
 * - 展示当前人格的头像、名称与分类标签，并作为人格卡的展开开关
 *
 * 设计特点:
 * - 头像独立成按钮，昵称与分类只作文本展示，不承担点击
 * - 展开态经 aria-expanded 透出，目标人格卡 id 由调用方给出
 * - 记忆页与规则页共用同一形态，各自传入自己的 DOM id
 */
import { computed } from 'vue'

import BaseTag from '../base/BaseTag.vue'
import type { PersonaSummary } from '../../core/api/dto'
import { personaCategory, personaInitial } from '../../core/memory/format'

const props = defineProps<{
  /** 头像按钮的 DOM id（供测试与焦点回收定位） */
  id: string
  /** 人格卡的 DOM id（aria-controls 指向） */
  controls: string
  persona: PersonaSummary | null
  open: boolean
}>()

defineEmits<{
  toggle: []
}>()

const name = computed(() => props.persona?.name ?? '未选择人格')
const initial = computed(() => personaInitial(props.persona?.name))
const category = computed(() =>
  props.persona ? personaCategory(props.persona.kind, props.persona.source) : null,
)
</script>

<template>
  <div class="persona-filter">
    <button
      :id="id"
      type="button"
      class="avatar"
      :aria-expanded="open ? 'true' : 'false'"
      aria-haspopup="dialog"
      :aria-controls="controls"
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
</template>

<style scoped>
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
  z-index: var(--z-local);
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
</style>
