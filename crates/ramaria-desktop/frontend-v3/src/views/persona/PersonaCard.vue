<script setup lang="ts">
/**
 * src/views/persona/PersonaCard.vue — 人格列表卡片
 *
 * 职责:
 * - 展示单个人格的类型、默认标记、来源、描述预览与更新时间
 *
 * 设计特点:
 * - 卡片整体可点，Enter / 空格与点击等价（沿用 BaseCard 的键盘约定）
 * - 头像优先用已保存图片，缺失或不可读时回退名称首字
 * - 描述缺失时给固定文案，保持卡片高度稳定
 */
import { computed, ref, watch } from 'vue'

import BaseCard from '../../components/base/BaseCard.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaFull } from '../../core/api/dto'
import { formatDateTime, smartTime } from '../../core/chat/time'
import { personaInitial } from '../../core/memory/format'
import { personaKindLabel, personaKindTone } from '../../core/persona/format'

const props = defineProps<{
  persona: PersonaFull
  isDefault: boolean
}>()

defineEmits<{
  open: []
}>()

const previewFailed = ref(false)

const initial = computed(() => personaInitial(props.persona.name))
const avatarSrc = computed(() => {
  const value = props.persona.avatar?.trim() ?? ''
  return value !== '' && !previewFailed.value ? value : ''
})
const kindTone = computed(() => personaKindTone(props.persona.kind))
const kindLabel = computed(() => personaKindLabel(props.persona.kind))
const description = computed(() => props.persona.description?.trim() ?? '')
const updatedText = computed(() => smartTime(props.persona.updated_at))
const updatedFull = computed(() => formatDateTime(props.persona.updated_at))

watch(
  () => props.persona.avatar,
  () => {
    previewFailed.value = false
  },
)
</script>

<template>
  <BaseCard clickable class="persona-card" @activate="$emit('open')">
    <div class="top">
      <span class="avatar" aria-hidden="true">
        <img v-if="avatarSrc" :src="avatarSrc" alt="" @error="previewFailed = true" />
        <template v-else>{{ initial }}</template>
      </span>
      <div class="info">
        <div class="name-row">
          <span class="name">{{ persona.name }}</span>
          <BaseTag :tone="kindTone">{{ persona.kind }}</BaseTag>
          <BaseTag v-if="isDefault" tone="brand">默认</BaseTag>
          <BaseTag v-if="!persona.is_active" tone="neutral">已停用</BaseTag>
        </div>
        <div class="uid" :title="persona.uid">UID {{ persona.uid }} · {{ persona.source }}</div>
      </div>
      <span class="arrow" aria-hidden="true">→</span>
    </div>

    <p class="desc" :class="{ 'is-empty': description === '' }">
      {{ description || '暂无描述' }}
    </p>

    <div class="meta">
      <span :title="updatedFull">更新于 {{ updatedText }}</span>
      <span>{{ kindLabel }}</span>
    </div>
  </BaseCard>
</template>

<style scoped>
.persona-card {
  gap: var(--space-10);
  min-height: 156px;
}

.top {
  display: flex;
  align-items: center;
  gap: var(--space-12);
}

.avatar {
  display: grid;
  flex: none;
  place-items: center;
  width: 44px;
  height: 44px;
  overflow: hidden;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  box-shadow: inset 0 0 0 1px var(--brand-line);
  color: var(--brand-text);
  font-size: var(--fs-h2);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.info {
  flex: 1;
  min-width: 0;
}

.name-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.name {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
  line-height: 1.3;
}

.uid {
  margin-top: var(--space-4);
  overflow: hidden;
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.arrow {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-body-lg);
  transition:
    color var(--dur-fast) var(--ease-out),
    transform var(--dur-fast) var(--ease-out);
}

.persona-card:hover .arrow {
  color: var(--brand);
  transform: translateX(2px);
}

.desc {
  display: -webkit-box;
  overflow: hidden;
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.62;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
}

.desc.is-empty {
  color: var(--text-3);
}

.meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-10);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.meta span:last-child {
  text-align: right;
}
</style>
