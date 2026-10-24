<script setup lang="ts">
/**
 * src/views/persona/PersonaDetailToolbar.vue — 人格详情工具栏
 *
 * 职责:
 * - 承载返回列表、当前人格标识、未保存提示与保存动作
 *
 * 设计特点:
 * - 名称与标记随草稿实时更新，未保存时以独立标记提示
 * - 保存按钮只在有差异且名称合法时可用，避免空提交
 * - 头像优先用已保存的图片，缺失或不可读时回退名称首字
 * - 返回键与卡片圆角切点对齐，行内高度固定 60px 不随内容跳变
 */
import { computed, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaFull } from '../../core/api/dto'
import { personaInitial } from '../../core/memory/format'
import { personaKindTone } from '../../core/persona/format'

const props = defineProps<{
  persona: PersonaFull
  name: string
  avatar: string
  isDefault: boolean
  dirty: boolean
  saving: boolean
  canSave: boolean
}>()

defineEmits<{
  back: []
  save: []
}>()

const previewFailed = ref(false)

const initial = computed(() => personaInitial(props.name || props.persona.name))
const kindTone = computed(() => personaKindTone(props.persona.kind))
const avatarSrc = computed(() => {
  const value = props.avatar.trim()
  return value !== '' && !previewFailed.value ? value : ''
})

watch(
  () => props.avatar,
  () => {
    previewFailed.value = false
  },
)
</script>

<template>
  <section class="toolbar">
    <BaseButton variant="secondary" size="sm" class="back" @click="$emit('back')">
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="m15 18-6-6 6-6" />
      </svg>
      人格
    </BaseButton>

    <span class="divider" aria-hidden="true" />

    <span class="avatar" aria-hidden="true">
      <img v-if="avatarSrc" :src="avatarSrc" alt="" @error="previewFailed = true" />
      <template v-else>{{ initial }}</template>
    </span>
    <div class="id">
      <span class="name">{{ name.trim() || '未命名' }}</span>
      <BaseTag :tone="kindTone">{{ persona.kind }}</BaseTag>
      <BaseTag v-if="isDefault" tone="brand">默认人格</BaseTag>
    </div>

    <span class="spacer" />

    <span v-if="dirty" class="dirty" role="status">未保存</span>
    <BaseButton
      variant="primary"
      :disabled="!canSave"
      :loading="saving"
      @click="$emit('save')"
    >
      保存修改
    </BaseButton>
  </section>
</template>

<style scoped>
.toolbar {
  position: sticky;
  top: 0;
  z-index: var(--z-sticky);
  display: flex;
  flex: none;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  min-height: 60px;
  /* 左内距取卡片圆角半径，返回键与圆角切点对齐 */
  padding: var(--space-10) var(--space-16) var(--space-10) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.back {
  gap: var(--space-4);
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

.divider {
  width: 1px;
  height: 22px;
  background: var(--border-strong);
}

.avatar {
  display: grid;
  flex: none;
  place-items: center;
  width: 36px;
  height: 36px;
  overflow: hidden;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  box-shadow: inset 0 0 0 1px var(--brand-line);
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

/* 昵称与标签同一行：小标签贴着昵称，横向平衡字号落差 */
.id {
  display: flex;
  flex: 0 1 auto;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
  min-width: 0;
}

.name {
  min-width: 0;
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
  line-height: 1.25;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.spacer {
  flex: 1;
}

.dirty {
  padding: 2px var(--space-8);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-tag);
  background: var(--warn-surface);
  color: var(--warn);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}
</style>
