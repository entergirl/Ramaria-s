<script setup lang="ts">
/**
 * src/views/persona/PersonaMetaCard.vue — 画像与元数据卡
 *
 * 职责:
 * - 只读展示 UID / 类型 / 来源 / 时间，呈现默认人格与画像状态
 * - 承载设为默认、重载画像、导出人格三个动作
 * - 提示词展开时压缩为单行 UID 摘要
 *
 * 设计特点:
 * - 元数据按固定行序呈现，长 UID 单行省略并保留完整值便于悬停查看
 * - 画像未生成与读取失败分开表述，动作仍在卡内，不弹新页
 * - 状态说明合并为至多三行：画像、默认标记、模型不可用提示
 * - 对话模型不可用时禁用重载并给出去处，编辑与导出不受影响
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaFull } from '../../core/api/dto'
import { formatDateTime } from '../../core/chat/time'
import type { TagTone } from '../../core/memory/format'
import { personaKindLabel } from '../../core/persona/format'
import { stateCopy } from '../../core/stateCopy'

const props = defineProps<{
  persona: PersonaFull
  isDefault: boolean
  defaultSaving: boolean
  defaultError: string
  profileChip: { text: string; tone: TagTone }
  profileNote: string
  profileDegraded: boolean
  reloading: boolean
  llmUnavailable: boolean
  compact: boolean
}>()

defineEmits<{
  'set-default': []
  reload: []
  export: []
}>()

const kindText = computed(() => `${props.persona.kind} · ${personaKindLabel(props.persona.kind)}`)
const createdText = computed(() => formatDateTime(props.persona.created_at))
const updatedText = computed(() => formatDateTime(props.persona.updated_at))
const isSelf = computed(() => props.persona.kind === 'user')
const defaultNote = computed(() => {
  if (props.defaultError !== '') {
    return '默认人格标记读取失败，当前标记可能不准确。'
  }
  if (isSelf.value) {
    return '本人不承担对话职能，不能设为默认对话人格。'
  }
  return props.isDefault
    ? '新会话与外部接入（绑定未覆盖时）默认使用该人格。'
    : '设为默认后，新会话与外部接入（绑定未覆盖时）默认使用该人格。'
})
const llmCopy = stateCopy('persona.degraded.llm')
</script>

<template>
  <BaseCard class="meta-card" :class="{ 'is-compact': compact }">
    <header class="section-head">
      <div class="head-text">
        <div class="section-title">画像与元数据</div>
        <div class="section-sub">只读字段由系统维护。</div>
      </div>
    </header>

    <div class="meta-list">
      <div class="meta-row">
        <span class="k">UID</span>
        <code :title="persona.uid">{{ persona.uid }}</code>
      </div>
      <div class="meta-row">
        <span class="k">类型</span>
        <span>{{ kindText }}</span>
      </div>
      <div class="meta-row">
        <span class="k">来源</span>
        <span>{{ persona.source }}</span>
      </div>
      <div class="meta-row">
        <span class="k">来源 ID</span>
        <span>{{ persona.ref_id || '—' }}</span>
      </div>
      <div class="meta-row">
        <span class="k">创建</span>
        <span>{{ createdText }}</span>
      </div>
      <div class="meta-row">
        <span class="k">更新</span>
        <span>{{ updatedText }}</span>
      </div>
    </div>

    <div class="status-box" :data-degraded="profileDegraded ? '' : undefined">
      <div class="status-line">
        <BaseTag :tone="isDefault ? 'brand' : 'neutral'">
          {{ isDefault ? '默认人格' : '未设为默认' }}
        </BaseTag>
        <BaseTag :tone="profileChip.tone">{{ profileChip.text }}</BaseTag>
      </div>
      <p class="status-note">{{ profileNote }}</p>
      <p class="status-note">{{ defaultNote }}</p>
      <p v-if="llmUnavailable" class="status-note is-warn">
        {{ llmCopy.title }}：{{ llmCopy.description }}
      </p>
    </div>

    <div class="actions">
      <BaseButton
        variant="secondary"
        size="sm"
        :disabled="isSelf || isDefault || defaultSaving"
        :loading="defaultSaving"
        @click="$emit('set-default')"
      >
        {{ isDefault ? '当前默认' : '设为默认' }}
      </BaseButton>
      <BaseButton
        variant="secondary"
        size="sm"
        :disabled="llmUnavailable"
        :loading="reloading"
        @click="$emit('reload')"
      >
        重载画像
      </BaseButton>
      <BaseButton variant="secondary" size="sm" @click="$emit('export')">导出人格</BaseButton>
    </div>

    <div class="compact-uid">
      <span class="compact-uid-label">UID</span>
      <code :title="persona.uid">{{ persona.uid }}</code>
    </div>
  </BaseCard>
</template>

<style scoped>
.meta-card {
  gap: var(--space-8);
  padding: var(--pad-card);
}

.section-head {
  display: flex;
  align-items: flex-start;
  gap: var(--space-8);
}

.head-text {
  min-width: 0;
}

.section-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
}

.section-sub {
  margin-top: 2px;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.5;
}

.meta-list {
  display: flex;
  flex-direction: column;
  padding: var(--space-4) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.meta-row {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: 5px 0;
  border-bottom: 1px dashed var(--border);
  font-size: var(--fs-small);
}

.meta-row:last-child {
  border-bottom: 0;
}

.meta-row .k {
  flex: none;
  width: 62px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.meta-row > span:last-child,
.meta-row code {
  min-width: 0;
  overflow: hidden;
  color: var(--text-2);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.meta-row code {
  padding: 1px var(--space-6);
  border: 1px solid var(--border);
  border-radius: var(--radius-check);
  background: var(--bg-card);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.status-box {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.status-box[data-degraded] {
  border-color: var(--warn-line);
  background: var(--warn-surface);
}

.status-line {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.status-note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.status-note.is-warn {
  color: var(--warn);
}

.actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.compact-uid {
  display: none;
}

/* ===== 压缩态：只留 UID 摘要 ===== */
.is-compact {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  height: 52px;
  padding: var(--space-8) var(--space-16);
  gap: var(--space-10);
}

.is-compact .section-head {
  flex: 1;
  min-width: 0;
}

.is-compact .section-sub {
  display: none;
}

.is-compact .meta-list,
.is-compact .status-box,
.is-compact .actions {
  display: none;
}

.is-compact .compact-uid {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
  max-width: 55%;
  padding: var(--space-6) var(--space-10);
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-input);
  background: var(--brand-surface);
}

.compact-uid-label {
  flex: none;
  color: var(--brand-text);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  letter-spacing: 0.04em;
}

.compact-uid code {
  min-width: 0;
  overflow: hidden;
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
