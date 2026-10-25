<script setup lang="ts">
/**
 * src/views/import/ImportFileStep.vue — 导入向导第一步（选择文件与参数）
 *
 * 职责:
 * - 文件行（文件名与路径 / 更换）、导入模式、导入侧、切割间隔与双方 persona 字段
 * - 展示「继续处理」入口与续跑规模提示
 *
 * 设计特点:
 * - 文件行与模式卡同屏常驻：未选文件时用同一行承担选择，不切到空态页
 * - 模式卡写明是否调用模型，避免误触全管线
 * - 单一主操作：分析文件居右下；更换文件只在文件行内
 * - 表单模型由父级持有；子组件按字段写入，父级负责提交归一与校验
 */

import BaseButton from '../../components/base/BaseButton.vue'
import BaseInput from '../../components/base/BaseInput.vue'
import BaseSelect from '../../components/base/BaseSelect.vue'
import { GAP_MAX, GAP_MIN, type ImportForm } from '../../core/import/form'
import { IMPORT_MODE_OPTIONS, IMPORT_SIDE_HINT, IMPORT_SIDE_OPTIONS } from '../../core/import/options'

const props = defineProps<{
  form: ImportForm
  fileName: string
  busy: boolean
  error: string
  resumable: boolean
  resumeSummary: string
  statusLoading: boolean
  statusError: string
}>()

const emit = defineEmits<{
  selectFile: []
  analyze: []
  continueImport: []
  retryStatus: []
}>()

/** 切割间隔输入：非法输入回落到下限，避免提交 0 或负数 */
function onGapInput(value: string): void {
  const parsed = Number.parseInt(value, 10)
  props.form.gapMinutes = Number.isFinite(parsed)
    ? Math.min(GAP_MAX, Math.max(GAP_MIN, parsed))
    : GAP_MIN
}

/** 导入侧选择：把下拉字符串收敛到联合类型 */
function onSideChange(value: string): void {
  props.form.side = value as ImportForm['side']
}
</script>

<template>
  <div class="step-body">
    <div v-if="error" class="strip strip--danger" role="alert">
      <span>{{ error }}</span>
    </div>

    <!-- 文件行：未选时承担选择，已选时展示名称与路径 -->
    <div class="file-row">
      <span class="file-icon" aria-hidden="true">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      </span>
      <div class="file-copy">
        <div class="file-name" :class="{ 'is-empty': !form.filePath }">
          {{ form.filePath ? fileName : '未选择文件' }}
        </div>
        <div class="file-meta" :title="form.filePath || undefined">
          {{
            form.filePath
              ? form.filePath
              : '支持 qq-chat-exporter v6.x 导出的 JSON；含图片时请保留与 JSON 同级的 resources/ 目录'
          }}
        </div>
      </div>
      <BaseButton
        v-if="form.filePath"
        variant="ghost"
        size="sm"
        :disabled="busy"
        @click="emit('selectFile')"
      >
        更换文件
      </BaseButton>
      <BaseButton v-else size="sm" :disabled="busy" @click="emit('selectFile')">
        选择文件
      </BaseButton>
    </div>

    <div class="mode-row" aria-label="导入模式">
      <button
        v-for="option in IMPORT_MODE_OPTIONS"
        :key="option.value"
        type="button"
        class="mode"
        :class="{ 'is-selected': form.mode === option.value }"
        :aria-pressed="form.mode === option.value"
        :disabled="busy"
        @click="form.mode = option.value"
      >
        <span class="mode-title">
          <span class="radio" aria-hidden="true" />
          {{ option.title }}
        </span>
        <span class="mode-desc">{{ option.description }}</span>
      </button>
    </div>

    <div class="fields">
      <BaseInput
        v-model="form.selfName"
        label="我方 Persona 名称"
        placeholder="留空沿用文件中的导出者名称"
        :disabled="busy"
      />
      <BaseInput
        v-model="form.selfUid"
        label="我方 Persona UID"
        placeholder="留空自动生成"
        :disabled="busy"
      />
      <BaseInput
        v-model="form.otherName"
        label="对方 Persona 名称"
        placeholder="留空沿用文件中的对方名称"
        :disabled="busy"
      />
      <BaseInput
        v-model="form.otherUid"
        label="对方 Persona UID"
        placeholder="留空自动生成"
        :disabled="busy"
      />
      <BaseSelect
        :model-value="form.side"
        label="导入侧"
        :options="IMPORT_SIDE_OPTIONS.map((item) => ({ ...item }))"
        :hint="IMPORT_SIDE_HINT"
        :disabled="busy"
        @update:model-value="onSideChange"
      />
      <BaseInput
        :model-value="String(form.gapMinutes)"
        label="Session 切割间隔（分钟）"
        type="number"
        :hint="`同一段对话中断超过该间隔即切分会话；允许 ${GAP_MIN}–${GAP_MAX}。`"
        :disabled="busy"
        @update:model-value="onGapInput"
      />
    </div>

    <!-- 继续处理入口：仅存在待办时出现 -->
    <div v-if="statusError" class="strip strip--warn" role="status">
      <span>{{ statusError }}</span>
      <BaseButton variant="ghost" size="sm" @click="emit('retryStatus')">重试</BaseButton>
    </div>
    <div v-else-if="resumable" class="resume-row">
      <div class="resume-copy">
        <div class="resume-title">
          检测到可继续处理的事项<span class="resume-text"> · {{ resumeSummary }}</span>
        </div>
        <div class="resume-hint">
          选择含 resources/ 的导出目录可回补图片；取消目录选择则只补摘要与画像。
        </div>
      </div>
      <BaseButton variant="secondary" size="sm" :disabled="busy" @click="emit('continueImport')">
        继续处理
      </BaseButton>
    </div>
    <div v-else-if="statusLoading" class="resume-loading" role="status">正在检查未完成的导入…</div>

    <div class="actions">
      <BaseButton :disabled="busy || !form.filePath" @click="emit('analyze')">分析文件</BaseButton>
    </div>
  </div>
</template>

<style scoped>
.step-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-14);
}

.strip {
  display: flex;
  align-items: center;
  gap: var(--space-12);
  padding: var(--space-10) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  font-size: var(--fs-small);
}

.strip--danger {
  border-color: var(--danger-line);
  background: var(--danger-surface);
  color: var(--danger);
}

.strip--warn {
  border-color: var(--warn-line);
  background: var(--warn-surface);
  color: var(--warn);
}

.strip .btn {
  margin-left: auto;
}

.file-row {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.file-icon {
  display: grid;
  flex: none;
  width: 32px;
  height: 32px;
  place-items: center;
  border-radius: var(--radius-input);
  background: var(--brand-surface);
  color: var(--brand-text);
}

.file-copy {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.file-name {
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-name.is-empty {
  color: var(--text-2);
  font-weight: var(--fw-medium);
}

.file-meta {
  overflow: hidden;
  color: var(--text-3);
  font-size: var(--fs-caption);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mode-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-12);
}

.mode {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  padding: var(--space-12) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
  box-shadow: var(--shadow-xs);
  color: var(--text-1);
  text-align: left;
  cursor: pointer;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    background-color var(--dur-fast) var(--ease-out);
}

.mode:hover:not(:disabled) {
  border-color: var(--border-strong);
}

.mode.is-selected {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

.mode:disabled {
  opacity: var(--opacity-disabled);
  cursor: not-allowed;
}

.mode-title {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
}

.mode-desc {
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.radio {
  display: inline-block;
  flex: none;
  width: 14px;
  height: 14px;
  border: 1.5px solid var(--border-strong);
  border-radius: var(--radius-full);
}

.mode.is-selected .radio {
  border-color: var(--brand);
  box-shadow: inset 0 0 0 3px var(--bg-card);
  background: var(--brand);
}

.fields {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-10) var(--space-14);
}

/* 本页字段行收紧：标签与控制件贴近，无提示时不预留空行 */
.fields :deep(.field) {
  gap: var(--space-4);
}

.fields :deep(.field-message) {
  min-height: 0;
  line-height: var(--lh-caption);
}

.resume-row {
  display: flex;
  align-items: center;
  gap: var(--space-12);
  padding: var(--space-10) var(--space-14);
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-inner);
  background: var(--brand-surface);
}

.resume-copy {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}

.resume-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.resume-text {
  color: var(--text-2);
  font-weight: var(--fw-regular);
}

.resume-hint,
.resume-loading {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
}

.actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-10);
}

@media (max-width: 720px) {
  .mode-row,
  .fields {
    grid-template-columns: 1fr;
  }
}
</style>
