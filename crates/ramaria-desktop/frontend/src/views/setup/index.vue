<script setup lang="ts">
/**
 * src/views/setup/index.vue — 首次配置视图
 *
 * 职责:
 * - 编排三步向导：对话模型 → 嵌入模型 → 确认并进入
 * - 承载步骤切换、未测试 / 未校验的二次确认、隐私确认模态与保存后的完成态
 *
 * 设计特点:
 * - 步骤状态与异步动作在 `useSetupWizard`，本组件只做布局与交互编排
 * - 三步走完即进入应用：第 3 步保存成功后直接刷新状态并回到上次视图，不设完成页
 * - 未测试 / 未校验不硬拦：给出影响说明，允许跳过（跳过则进入降级）
 * - 保存与进入期间锁定上一步与下一步，避免脏值竞态；底部按钮高度不跳变
 */

import { computed, onMounted, reactive, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCheckbox from '../../components/base/BaseCheckbox.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import FullscreenShell from '../../components/shell/FullscreenShell.vue'
import { SETUP_STEPS, summaryRows } from '../../core/setupWizard'
import StepConfirm from './StepConfirm.vue'
import StepEmbedding from './StepEmbedding.vue'
import StepModel from './StepModel.vue'
import SetupStepper from './SetupStepper.vue'
import { useSetupWizard } from './useSetupWizard'

const {
  step,
  fields,
  errors,
  testing,
  connectionState,
  connectionMessage,
  validating,
  embeddingValidation,
  embeddingReady,
  embeddingSkipped,
  privacyChoice,
  privacyLoading,
  submitting,
  submitError,
  entering,
  loadInitialState,
  setMode,
  validateModelStep,
  testConnection,
  validateEmbedding,
  skipEmbedding,
  loadPrivacyStatus,
  acceptPrivacy,
  submit,
  enterApp,
} = useSetupWizard()

/** 未测试 / 未校验的二次确认 */
const hint = ref<'' | 'test' | 'embedding'>('')
/** 隐私确认模态（草稿态，确认后才写回） */
const privacyOpen = ref(false)
const privacyDraft = reactive({ accepted: false, persistent: true })

const nextLabel = computed(() => (step.value === 3 ? '保存并进入 Ramaria' : '下一步'))
const canGoPrev = computed(() => step.value === 2 || step.value === 3)
/** 保存与进入应用期间统一锁住表单 */
const locked = computed(() => submitting.value || entering.value)

const rows = computed(() =>
  summaryRows({
    fields,
    embeddingReady: embeddingReady.value,
    embeddingSkipped: embeddingSkipped.value,
  }),
)

const apiBaseUrl = computed(() => fields.apiUrl.trim())

/** 页脚状态文案：只描述当前步的完成情况，保证操作者知道「还差什么」 */
const footState = computed(() => {
  if (step.value === 1) {
    return connectionState.value === 'ok' ? '连接已通过' : '建议先测试连接'
  }
  if (step.value === 2) {
    if (embeddingReady.value) {
      return '模型校验通过'
    }
    return embeddingSkipped.value ? '已跳过 · 降级模式' : '未校验模型'
  }
  if (fields.mode !== 'online') {
    return ''
  }
  return privacyChoice.accepted ? '隐私已确认' : '线上模式待隐私确认'
})

onMounted(() => {
  void loadInitialState()
})

watch(step, (value) => {
  if (value === 3) {
    void loadPrivacyStatus()
  }
})

function next(): void {
  if (locked.value) {
    return
  }
  if (step.value === 1) {
    if (validateModelStep() > 0) {
      return
    }
    if (connectionState.value !== 'ok') {
      hint.value = 'test'
      return
    }
    step.value = 2
    return
  }
  if (step.value === 2) {
    if (embeddingReady.value) {
      step.value = 3
      return
    }
    hint.value = 'embedding'
    return
  }
  if (step.value === 3) {
    void finish()
  }
}

/** 保存配置并进入应用：任一步失败都留在当前页 */
async function finish(): Promise<void> {
  if (locked.value) {
    return
  }
  // 线上后端未确认隐私时先拉起确认，不把用户丢在一行错误提示上
  if (fields.mode === 'online' && !privacyChoice.accepted) {
    openPrivacy()
    return
  }
  const ok = await submit()
  if (ok) {
    await enterApp()
  }
}

function prev(): void {
  if (locked.value) {
    return
  }
  if (step.value === 2 || step.value === 3) {
    step.value -= 1
  }
}

function skipTest(): void {
  hint.value = ''
  step.value = 2
}

function skipEmbeddingAndContinue(): void {
  hint.value = ''
  skipEmbedding()
  step.value = 3
}

function goto(stepId: number): void {
  if (stepId >= 1 && stepId < step.value) {
    step.value = stepId
  }
}

function openPrivacy(): void {
  privacyDraft.accepted = privacyChoice.accepted
  privacyDraft.persistent = privacyChoice.persistent
  privacyOpen.value = true
}

function confirmPrivacyDraft(): void {
  acceptPrivacy(privacyDraft.persistent)
  privacyOpen.value = false
}
</script>

<template>
  <FullscreenShell
    data-view-state="default"
    :data-restricted="locked ? 'locked' : undefined"
    :aria-busy="locked ? 'true' : undefined"
  >
    <section class="wizard" :class="{ 'is-online': fields.mode === 'online' }">
      <header class="wizard-head">
        <div class="wizard-title-row">
          <div class="wizard-title-copy">
            <h1 class="wizard-title">首次配置</h1>
            <p class="wizard-sub">完成对话模型与嵌入模型配置，即可开始使用 Ramaria。</p>
          </div>
          <span class="step-count">第 {{ step }} / {{ SETUP_STEPS.length }} 步</span>
        </div>
        <SetupStepper :current="step" :labels="SETUP_STEPS" />
      </header>

      <div :key="step" class="wizard-body">
        <StepModel
          v-if="step === 1"
          :mode="fields.mode"
          v-model:local-url="fields.localUrl"
          v-model:local-model="fields.localModel"
          v-model:api-url="fields.apiUrl"
          v-model:api-key="fields.apiKey"
          v-model:api-model="fields.apiModel"
          v-model:vision="fields.visionSupported"
          :errors="errors"
          :testing="testing"
          :connection-state="connectionState"
          :connection-message="connectionMessage"
          @update:mode="setMode"
          @test="testConnection"
        />
        <StepEmbedding
          v-else-if="step === 2"
          v-model:path="fields.embeddingPath"
          :errors="errors"
          :validating="validating"
          :validation="embeddingValidation"
          :skipped="embeddingSkipped"
          @validate="validateEmbedding"
          @skip="skipEmbeddingAndContinue"
        />
        <StepConfirm
          v-else
          :rows="rows"
          :mode="fields.mode"
          :privacy-accepted="privacyChoice.accepted"
          :privacy-loading="privacyLoading"
          :submit-error="submitError"
          :locked="locked"
          @goto="goto"
          @privacy="openPrivacy"
        />
      </div>

      <footer class="wizard-foot">
        <BaseButton v-if="canGoPrev" variant="ghost" :disabled="locked" @click="prev">
          上一步
        </BaseButton>
        <span class="spacer" />
        <span class="foot-state" role="status" aria-live="polite">{{ footState }}</span>
        <BaseButton
          variant="primary"
          :loading="locked"
          :disabled="testing || validating"
          @click="next"
        >
          {{ nextLabel }}
        </BaseButton>
      </footer>
    </section>

    <BaseModal :open="hint === 'test'" title="未测试连接" @close="hint = ''">
      <p class="modal-text">
        还没有测试{{ fields.mode === 'online' ? '线上 API' : '本地推理服务' }}连接，配置有误时无法正常对话。
      </p>
      <p class="modal-text">可以先点「测试连接」确认配置，或跳过测试直接继续。</p>
      <template #foot>
        <BaseButton variant="ghost" @click="hint = ''">取消</BaseButton>
        <span class="spacer" />
        <BaseButton variant="primary" @click="skipTest">跳过测试，继续</BaseButton>
      </template>
    </BaseModal>

    <BaseModal :open="hint === 'embedding'" title="未校验嵌入模型" @close="hint = ''">
      <p class="modal-text">跳过将进入降级模式，以下能力受影响：</p>
      <ul class="modal-list">
        <li>向量检索不可用，仅保留关键词与图谱通道</li>
        <li>对话与记忆浏览不受影响</li>
        <li>可随时在「设置 → 嵌入与视觉」补配</li>
      </ul>
      <template #foot>
        <BaseButton variant="ghost" @click="hint = ''">返回校验</BaseButton>
        <span class="spacer" />
        <BaseButton variant="primary" @click="skipEmbeddingAndContinue">跳过，继续</BaseButton>
      </template>
    </BaseModal>

    <BaseModal :open="privacyOpen" title="线上后端隐私确认" @close="privacyOpen = false">
      <p class="modal-text">
        对话内容（含记忆上下文）将发送至云端模型服务（{{ apiBaseUrl }}）处理；开启图片理解时，聊天记录中的图片也会随理解请求发送。
      </p>
      <p class="modal-text">敏感内容建议改用本地部署；确认后仍可在设置中查看范围与时间。</p>
      <div class="modal-checks">
        <BaseCheckbox v-model="privacyDraft.accepted" label="我已知晓上述数据范围与去向" />
        <BaseCheckbox v-model="privacyDraft.persistent" label="下次不再提醒" />
      </div>
      <template #foot>
        <BaseButton variant="ghost" @click="privacyOpen = false">取消</BaseButton>
        <span class="spacer" />
        <BaseButton variant="primary" :disabled="!privacyDraft.accepted" @click="confirmPrivacyDraft">
          确认
        </BaseButton>
      </template>
    </BaseModal>
  </FullscreenShell>
</template>

<style scoped>
.wizard {
  /* 版式档位：默认档（窗口足够高）；矮窗 / 窄窗由下方媒体查询逐档收紧 */
  --wz-gap: var(--space-20);
  --wz-panel: var(--space-14);
  --wz-grid-row: var(--space-10);
  --wz-status: var(--space-10);
  --wz-mode: var(--space-14);
  --wz-reserve: 18px;

  display: flex;
  flex-direction: column;
  gap: var(--wz-gap);
  /* 宽随窗口收缩、上限 800：右侧留 340 给浮动圆键，与 setup-demo 的自适应卡片一致 */
  width: min(800px, calc(100% - 340px));
  /* 高度随内容收缩，且不越过舞台的可用高度（上 32 + 下 24） */
  max-height: calc(100vh - var(--space-32) - var(--space-24));
  margin: 0 auto;
  padding: var(--space-24) var(--space-32) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-md);
}

/* 线上模式比本地多一整行字段：默认档直接降到紧凑档，保证 720 高窗口一屏显示 */
.wizard.is-online {
  --wz-gap: var(--space-16);
  --wz-panel: var(--space-10);
  --wz-grid-row: var(--space-6);
  --wz-status: var(--space-8);
  --wz-mode: var(--space-12);
  --wz-reserve: 0px;
}

.wizard-head {
  display: flex;
  flex: none;
  flex-direction: column;
  gap: var(--space-16);
}

.wizard-title-row {
  display: flex;
  align-items: flex-start;
  gap: var(--space-16);
}

.wizard-title-copy {
  min-width: 0;
}

.wizard-title {
  font-family: var(--font-title);
  font-size: var(--fs-h1);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h1);
}

.wizard-sub {
  margin-top: var(--space-4);
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.step-count {
  flex: none;
  margin-left: auto;
  padding: var(--space-4) var(--space-12);
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-tag);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-small);
  white-space: nowrap;
}

.wizard-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  animation: panel-in var(--dur-slow) var(--ease-out);
}

@keyframes panel-in {
  from {
    opacity: 0;
    transform: translateY(10px);
  }
}

.wizard-foot {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-10);
  padding-top: var(--space-4);
  border-top: 1px dashed var(--border);
}

.foot-state {
  color: var(--text-3);
  font-size: var(--fs-small);
}

.spacer {
  flex: 1;
}

/* ===== 本页控件尺寸 =====
   demo 的输入 40、主按钮 38、次级 31；基础组件默认 32 / 32 / 26。
   仅向导页放大，不改基础组件、也不改设计规范的全站取值。 */
.wizard :deep(.field-input) {
  height: 40px;
  padding: 0 var(--space-12);
}

.wizard :deep(.btn) {
  height: 38px;
  padding: 0 var(--space-20);
}

.wizard :deep(.btn--sm) {
  height: 31px;
  padding: 0 var(--space-12);
}

.wizard :deep(.btn--icon) {
  width: 38px;
  padding: 0;
}

/* ===== 档位消费：面板间距 / 字段行距 / 状态条与模式卡内边距 ===== */
.wizard :deep(.panel) {
  gap: var(--wz-panel);
}

.wizard :deep(.field-grid) {
  row-gap: var(--wz-grid-row);
}

.wizard :deep(.field-message) {
  min-height: var(--wz-reserve);
}

.wizard :deep(.inline-status) {
  padding-top: var(--wz-status);
  padding-bottom: var(--wz-status);
}

.wizard :deep(.mode-card) {
  padding-top: var(--wz-mode);
  padding-bottom: var(--wz-mode);
}

/* ===== 窗口变窄：不再为浮动圆键预留右侧 340 ===== */
@media (max-width: 900px) {
  .wizard {
    width: min(800px, 100%);
  }
}

/* ===== 矮窗：逐档压缩间距，保证内容仍在一屏内显示（不出现正文滚动） ===== */
@media (max-height: 700px) {
  .wizard {
    --wz-gap: var(--space-14);
    --wz-panel: var(--space-12);
    --wz-status: var(--space-8);
    --wz-mode: var(--space-12);
    padding: var(--space-20) var(--space-24) var(--space-16);
  }

  /* 线上模式再紧一档 */
  .wizard.is-online {
    --wz-gap: var(--space-10);
    --wz-panel: var(--space-10);
    --wz-grid-row: var(--space-4);
    --wz-status: var(--space-4);
    --wz-mode: var(--space-10);
  }

  .wizard-head {
    gap: var(--space-12);
  }

  .wizard :deep(.note) {
    padding-top: var(--space-8);
    padding-bottom: var(--space-8);
  }

  .wizard :deep(.summary-row) {
    padding-top: var(--space-6);
    padding-bottom: var(--space-6);
  }
}

/* 矮窗或窄窗：卡片占满宽度后纵向更长，统一降为紧凑排版 */
@media (max-width: 900px), (max-height: 600px) {
  .wizard {
    --wz-gap: var(--space-10);
    --wz-panel: var(--space-10);
    --wz-status: var(--space-6);
    --wz-mode: var(--space-10);
    --wz-reserve: 0px;
    padding: var(--space-16) var(--space-20) var(--space-14);
  }

  /* 极矮窗口先收起说明性文案，字段与按钮的信息密度不变 */
  .wizard-sub,
  .wizard :deep(.panel-sub) {
    display: none;
  }

  .wizard-head {
    gap: var(--space-10);
  }

  .wizard :deep(.summary-row) {
    padding-top: var(--space-4);
    padding-bottom: var(--space-4);
  }

  .wizard :deep(.note) {
    padding: var(--space-6) var(--space-12);
  }
}

/* 极矮或窄窗：收起模式说明（线上多一行字段，靠它换回高度） */
@media (max-width: 900px), (max-height: 560px) {
  .wizard.is-online :deep(.mode-desc) {
    display: none;
  }
}

@media (max-height: 520px) {
  .wizard {
    --wz-gap: var(--space-8);
    max-height: calc(100vh - var(--space-32));
    padding: var(--space-12) var(--space-16) var(--space-10);
  }

  /* 步数徽章已标明进度，极矮窗口不再重复渲染步骤条 */
  .wizard :deep(.steps) {
    display: none;
  }

  .wizard.is-online {
    --wz-status: var(--space-4);
  }

  .wizard-title {
    font-size: var(--fs-h3);
    line-height: var(--lh-h3);
  }

  .wizard :deep(.panel-title) {
    font-size: var(--fs-body);
    line-height: var(--lh-body);
  }

  /* 摘要行已写明降级结论，极矮窗口不再重复整段说明 */
  .wizard :deep(.note) {
    display: none;
  }
}

.modal-text {
  margin: 0 0 var(--space-8);
  color: var(--text-2);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
}

.modal-list {
  margin: 0;
  padding-left: var(--space-20);
  color: var(--text-2);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
}

.modal-checks {
  display: flex;
  flex-direction: column;
  margin-top: var(--space-8);
}

@media (prefers-reduced-motion: reduce) {
  .wizard-body {
    animation: none;
  }
}
</style>
