<script setup lang="ts">
/**
 * src/views/import/index.vue — 导入视图入口
 *
 * 职责:
 * - 承载四步向导布局与步骤切换，挂载状态属性供测试与画廊断言
 * - 数据、动作与派生全部来自 `useImportView`
 *
 * 设计特点:
 * - 单卡片单列（工作台宽度），与设计稿的漂浮工作台一致
 * - `data-view-state` / `data-restricted` / `data-degraded` 只在此处声明
 * - 视图不直读其他视图 store：完成动作只经一次性跳转参数
 * - 卡片承载背景与描边，内容间距由本视图收紧
 */

import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import ImportDoneStep from './ImportDoneStep.vue'
import ImportFileStep from './ImportFileStep.vue'
import ImportPreviewStep from './ImportPreviewStep.vue'
import ImportRunStep from './ImportRunStep.vue'
import ImportStepper from './ImportStepper.vue'
import {
  IMPORT_STEP_LABELS,
  stepNumber,
  useImportView,
} from './useImportView'

const {
  step,
  form,
  report,
  result,
  analyzing,
  submitting,
  error,
  statusLoading,
  statusError,
  pendingContinue,
  job,
  busy,
  jobFailed,
  continueJob,
  viewState,
  restricted,
  degraded,
  progress,
  stageViews,
  fileName,
  modeText,
  reportStats,
  resultStats,
  doneStats,
  continueStats,
  reportMembers,
  resultMembers,
  timeRange,
  l1Failed,
  l1Total,
  visionFailed,
  skippedAttachments,
  resumable,
  resumeSummary,
  selectFile,
  changeFile,
  analyze,
  backToSelect,
  confirmImport,
  retryRun,
  continueImport,
  continueWithoutDir,
  importAnother,
  goMemory,
  goChat,
  loadResumeStatus,
} = useImportView()

const stepNo = computed(() => stepNumber(step.value))

/** 完成页统计：导入结果 + L1 统计 + 续跑统计（互斥出现） */
const doneMetrics = computed(() => [
  ...resultStats.value,
  ...doneStats.value,
  ...continueStats.value,
])

const canChat = computed(() => (result.value?.session_ids?.length ?? 0) > 0)
const canMemory = computed(
  () => !!(result.value?.persona_uid || result.value?.other_persona_uid),
)

const runMessage = computed(() => job.value?.message ?? '')
const runError = computed(() => (jobFailed.value ? error.value : ''))
</script>

<template>
  <BaseWorkbench>
    <section
      class="import"
      :data-view-state="viewState"
      :data-restricted="restricted"
      :data-degraded="degraded"
      :aria-busy="busy ? 'true' : undefined"
    >
      <BaseCard class="wizard">
        <header class="wizard-head">
          <h1 class="wizard-title">数据导入</h1>
          <p class="wizard-sub">
            QQ 聊天记录（qq-chat-exporter v6.x JSON）。文件只在本机解析，不上传。
          </p>
        </header>

        <ImportStepper :current="stepNo" :labels="IMPORT_STEP_LABELS" />

        <ImportFileStep
          v-if="step === 'select'"
          :form="form"
          :file-name="fileName"
          :busy="busy"
          :error="error"
          :resumable="resumable"
          :resume-summary="resumeSummary"
          :status-loading="statusLoading"
          :status-error="statusError"
          @select-file="selectFile"
          @analyze="analyze"
          @continue-import="continueImport"
          @retry-status="loadResumeStatus"
        />

        <ImportPreviewStep
          v-else-if="step === 'preview'"
          :report="report"
          :analyzing="analyzing"
          :error="error"
          :busy="busy"
          :stats="reportStats"
          :members="reportMembers"
          :time-range="timeRange"
          :file-name="fileName"
          :mode-text="modeText"
          @back="backToSelect"
          @confirm="confirmImport"
          @retry="analyze"
        />

        <ImportRunStep
          v-else-if="step === 'run'"
          :continue-job="continueJob"
          :result="result"
          :progress="progress"
          :stages="stageViews"
          :message="runMessage"
          :error="runError"
          :job-failed="jobFailed"
          :submitting="submitting"
          @retry="retryRun"
          @change-file="changeFile"
        />

        <ImportDoneStep
          v-else
          :continue-job="continueJob"
          :result="result"
          :metrics="doneMetrics"
          :l1-failed="l1Failed"
          :l1-total="l1Total"
          :vision-failed="visionFailed"
          :skipped-attachments="skippedAttachments"
          :members="resultMembers"
          :time-range="timeRange"
          :can-chat="canChat"
          :can-memory="canMemory"
          @another="importAnother"
          @chat="goChat"
          @memory="goMemory"
          @continue-import="continueImport"
        />
      </BaseCard>
    </section>

    <BaseModal
      :open="pendingContinue"
      title="未选择导出目录"
      @close="pendingContinue = false"
    >
      <p class="modal-copy">
        没有选择导出目录时，图片附件不会被回补；仍可补跑缺失的摘要与画像。
      </p>
      <template #foot>
        <span class="modal-spacer" />
        <BaseButton variant="ghost" @click="pendingContinue = false">取消</BaseButton>
        <BaseButton @click="continueWithoutDir">仅补摘要与画像</BaseButton>
      </template>
    </BaseModal>
  </BaseWorkbench>
</template>

<style scoped>
.import {
  display: flex;
  flex-direction: column;
  width: 100%;
  min-height: 0;
}

.wizard {
  gap: var(--space-14);
}

.wizard-head {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.wizard-title {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-h2);
  font-weight: var(--fw-bold);
  letter-spacing: -0.01em;
}

.wizard-sub {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-small);
}

.modal-copy {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
}

.modal-spacer {
  flex: 1;
}
</style>
