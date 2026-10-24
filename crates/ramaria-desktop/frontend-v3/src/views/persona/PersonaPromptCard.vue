<script setup lang="ts">
/**
 * src/views/persona/PersonaPromptCard.vue — 人格提示词查看器
 *
 * 职责:
 * - 折叠时展示静态段摘要与字数；展开时按整段正文阅读
 * - 提供复制、模板 / 编译预览切换与示例输入（全量档需输入）
 *
 * 设计特点:
 * - 折叠态就有内容可读，不必先展开才知道提示词长什么样
 * - 展开为卡内就地阅读，不弹模态；正文区占满剩余高度，页脚固定
 * - 编译预览只读、不写库：给出示例输入后才生成含召回的提示词
 * - 复制失败改提示手动选择，不阻塞阅读
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import type { PersonaPromptPreview } from '../../core/api/dto'

const props = defineProps<{
  open: boolean
  tier: 'static' | 'full'
  input: string
  staticPreview: PersonaPromptPreview | null
  fullPreview: PersonaPromptPreview | null
  loading: boolean
  error: string
  hint: string
  copied: boolean
}>()

const emit = defineEmits<{
  toggle: []
  'update:tier': [value: 'static' | 'full']
  'update:input': [value: string]
  request: []
  copy: []
}>()

/** 当前生效的预览：折叠态恒为静态段 */
const current = computed<PersonaPromptPreview | null>(() => {
  if (!props.open) {
    return props.staticPreview
  }
  return props.tier === 'full' ? props.fullPreview : props.staticPreview
})

const text = computed(() => current.value?.system_prompt ?? '')
const canCopy = computed(() => text.value.length > 0)
const chars = computed(() => current.value?.chars ?? 0)

const footLabel = computed(() => {
  if (props.open && props.tier === 'full') {
    return '编译预览 · 含示例召回'
  }
  if (props.open) {
    return '静态段 · 不含记忆召回'
  }
  return '模板 · 运行时填充记忆与规则'
})

const previewPlaceholder = computed(() => {
  if (props.loading) {
    return '读取中…'
  }
  if (props.error !== '') {
    return props.error
  }
  return '该人格暂时没有可预览的提示词。'
})

const showHint = computed(
  () => props.open && props.tier === 'full' && props.fullPreview === null && !props.loading,
)

function onInput(event: Event): void {
  emit('update:input', (event.target as HTMLInputElement).value)
}

function onTier(): void {
  emit('update:tier', props.tier === 'full' ? 'static' : 'full')
}
</script>

<template>
  <BaseCard class="prompt-card" :class="{ 'is-open': open }">
    <header class="section-head">
      <div class="head-text">
        <div class="section-title">人格提示词</div>
        <div class="section-sub">只读预览；模板会与记忆、规则一起编译为运行时提示词。</div>
      </div>
      <span class="spacer" />
      <div class="actions">
        <BaseButton v-if="open && canCopy" variant="ghost" size="sm" @click="$emit('copy')">
          {{ copied ? '已复制' : '复制' }}
        </BaseButton>
        <BaseButton
          v-if="open"
          variant="ghost"
          size="sm"
          :class="{ 'is-active': tier === 'full' }"
          @click="onTier"
        >
          {{ tier === 'full' ? '查看模板' : '编译预览' }}
        </BaseButton>
        <BaseButton
          variant="secondary"
          size="sm"
          aria-controls="persona-prompt-body"
          :aria-expanded="open ? 'true' : 'false'"
          @click="$emit('toggle')"
        >
          {{ open ? '收起提示词' : '查看提示词' }}
        </BaseButton>
      </div>
    </header>

    <div
      v-if="!open"
      class="prompt-preview"
      :class="{ 'is-state': text === '', 'is-error': text === '' && error !== '' }"
    >
      {{ text || previewPlaceholder }}
    </div>

    <div v-else id="persona-prompt-body" class="prompt-body">
      <div v-if="tier === 'full'" class="full-block">
        <div class="full-row">
          <label class="full-label" for="persona-prompt-input">示例输入</label>
          <input
            id="persona-prompt-input"
            class="full-input"
            type="text"
            autocomplete="off"
            :value="input"
            placeholder="输入一句用户可能说的话，用于生成含召回的预览"
            @input="onInput"
          />
          <BaseButton variant="secondary" size="sm" :loading="loading" @click="$emit('request')">
            生成预览
          </BaseButton>
        </div>
        <p v-if="hint !== ''" class="full-hint">{{ hint }}</p>
      </div>

      <div v-if="loading && text === ''" class="prompt-state">读取中…</div>

      <div v-else-if="error !== ''" class="prompt-error" role="alert">
        <span>{{ error }}</span>
        <BaseButton variant="ghost" size="sm" @click="$emit('request')">重试</BaseButton>
      </div>

      <div v-else-if="showHint" class="prompt-state">
        编译预览会按示例输入做一次记忆召回；给出示例后生成。
      </div>

      <pre v-else class="prompt-text">{{ text }}</pre>
    </div>

    <div class="prompt-foot">
      <span>{{ footLabel }}</span>
      <span class="spacer" />
      <span class="chars">{{ chars }} 字</span>
    </div>
  </BaseCard>
</template>

<style scoped>
.prompt-card {
  grid-column: 1 / -1;
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

.spacer {
  flex: 1;
}

.actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

/* 编译预览为开时着品牌底色，与另两个动作区分 */
.actions :deep(.is-active) {
  background: var(--brand-surface);
  color: var(--brand-text);
}

.prompt-preview {
  max-height: 76px;
  overflow: auto;
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
}

.prompt-preview.is-state {
  color: var(--text-3);
  font-family: inherit;
}

.prompt-preview.is-error {
  color: var(--danger);
}

.prompt-body {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-10);
  min-height: 0;
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.full-block {
  display: flex;
  flex: none;
  flex-direction: column;
  gap: var(--space-4);
}

.full-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.full-hint {
  margin: 0;
  color: var(--warn);
  font-size: var(--fs-caption);
}

.full-label {
  flex: none;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
}

.full-input {
  flex: 1;
  min-width: 220px;
  height: 30px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-small);
}

.full-input:focus-visible {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

.prompt-text {
  flex: 1;
  min-height: 0;
  max-height: 360px;
  margin: 0;
  padding: var(--space-10) var(--space-12);
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
}

.prompt-foot {
  display: flex;
  flex-wrap: wrap;
  flex: none;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.chars {
  font-family: var(--font-mono);
}

.prompt-state {
  flex: 1;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.prompt-error {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  color: var(--danger);
  font-size: var(--fs-small);
}

/* ===== 展开态：卡片撑满固定高度，正文占满 ===== */
.is-open {
  height: 470px;
}

.is-open .prompt-text {
  max-height: none;
}
</style>
