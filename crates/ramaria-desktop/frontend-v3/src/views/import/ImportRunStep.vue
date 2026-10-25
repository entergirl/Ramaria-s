<script setup lang="ts">
/**
 * src/views/import/ImportRunStep.vue — 导入向导第三步（执行与进度）
 *
 * 职责:
 * - 展示阶段条、阶段内百分比 / 计数 / 预计剩余时间与阶段描述
 * - 导入中断时给出重试与更换文件入口
 *
 * 设计特点:
 * - L0 写入阶段没有进度事件，用不定进度 + 明确文案，不伪造百分比
 * - 阶段条按导入模式裁剪；已完成阶段打勾
 * - 进度组件使用原生 progress，不写内联样式
 */

import BaseButton from '../../components/base/BaseButton.vue'
import BaseProgress from '../../components/base/BaseProgress.vue'
import type { ImportResult } from '../../core/api/dto'
import type { ImportStage, ImportStageState } from '../../core/import/progress'

defineProps<{
  continueJob: boolean
  result: ImportResult | null
  progress: { phaseLabel: string; percent: number | null; counts: string; etaText: string } | null
  stages: (ImportStage & { state: ImportStageState })[]
  message: string
  error: string
  jobFailed: boolean
  submitting: boolean
}>()

const emit = defineEmits<{
  retry: []
  changeFile: []
}>()
</script>

<template>
  <div class="step-body">
    <header class="run-head">
      <div class="run-title">{{ continueJob ? '正在继续处理' : '正在导入' }}</div>
      <div class="run-state">
        <span v-if="jobFailed" class="tag tag--danger">已中断</span>
        <span v-else-if="submitting" class="tag tag--brand">写入中</span>
        <span v-else class="tag tag--brand">处理中</span>
      </div>
    </header>

    <ol class="stage-row" aria-label="处理阶段">
      <li
        v-for="stage in stages"
        :key="stage.key"
        class="stage"
        :data-state="stage.state"
        :aria-current="stage.state === 'active' ? 'step' : undefined"
      >
        <span class="stage-dot" aria-hidden="true" />
        <span class="stage-label">{{ stage.label }}</span>
      </li>
    </ol>

    <BaseProgress
      :value="progress?.percent ?? undefined"
      :label="progress?.phaseLabel ?? (result ? '准备后续处理' : '写入对话记录')"
      :state="jobFailed ? 'failed' : 'running'"
    />

    <div class="run-meta">
      <span v-if="progress?.counts">已完成 {{ progress.counts }}</span>
      <span v-if="progress?.etaText">{{ progress.etaText }}</span>
      <span v-if="!progress?.counts && submitting">解析与写入可能需要数十秒，请稍候</span>
    </div>

    <p v-if="jobFailed && error" class="run-error" role="alert">{{ error }}</p>
    <p v-else-if="message" class="run-message">{{ message }}</p>
    <p v-else-if="result" class="run-message">
      已写入 {{ result.sessions_written }} 个会话 · {{ result.messages_written }} 条消息，正在启动后续处理…
    </p>

    <div class="run-note">
      离开此页不会中断处理；完成后会收到通知，回到本页可查看统计。
    </div>

    <div v-if="jobFailed" class="actions">
      <BaseButton :disabled="submitting" @click="emit('retry')">重试</BaseButton>
      <BaseButton variant="ghost" :disabled="submitting" @click="emit('changeFile')">
        更换文件
      </BaseButton>
    </div>
  </div>
</template>

<style scoped>
.step-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-16);
}

.run-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-12);
}

.run-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.tag {
  padding: 2px 8px;
  border-radius: var(--radius-tag);
  font-size: var(--fs-caption);
}

.tag--brand {
  background: var(--brand-surface);
  color: var(--brand-text);
}

.tag--danger {
  background: var(--danger-surface);
  color: var(--danger);
}

.stage-row {
  display: flex;
  gap: var(--space-8);
  margin: 0;
  padding: 0;
  list-style: none;
}

.stage {
  display: flex;
  flex: 1;
  align-items: center;
  gap: var(--space-6);
  padding: var(--space-6) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  color: var(--text-3);
  font-size: var(--fs-small);
  white-space: nowrap;
}

.stage[data-state='active'] {
  border-color: var(--brand-line);
  background: var(--brand-surface);
  color: var(--text-1);
}

.stage[data-state='done'] {
  border-color: var(--ok-line);
  color: var(--text-2);
}

.stage-dot {
  display: inline-block;
  flex: none;
  width: 8px;
  height: 8px;
  border-radius: var(--radius-full);
  background: var(--border-strong);
}

.stage[data-state='active'] .stage-dot {
  background: var(--brand);
}

.stage[data-state='done'] .stage-dot {
  background: var(--ok);
}

.stage-label {
  overflow: hidden;
  text-overflow: ellipsis;
}

.run-meta {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-12);
  color: var(--text-3);
  font-size: var(--fs-small);
}

.run-message,
.run-error {
  margin: 0;
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.run-message {
  color: var(--text-2);
}

.run-error {
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-inner);
  background: var(--danger-surface);
  color: var(--danger);
  word-break: break-word;
}

.run-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.actions {
  display: flex;
  gap: var(--space-10);
}

@media (max-width: 720px) {
  .stage-row {
    flex-wrap: wrap;
  }

  .stage {
    flex: 1 1 45%;
  }
}
</style>
