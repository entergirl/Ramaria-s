<script setup lang="ts">
/**
 * src/views/rules/index.vue — 规则视图入口
 *
 * 职责:
 * - 编排规则列表与详情两个页内形态
 * - 派生列表主态（加载 / 错误 / 空 / 默认）与降级条
 *
 * 设计特点:
 * - 内容区固定 800×600 并居中，超出部分在容器内滚动并在边缘渐隐
 * - 列表为双列栅格，留白收紧；详情为左右两列等高，证据链与参数卡各自内滚
 * - 详情即编辑：规则文本在详情页的文本框里改，保存动作在工具栏
 * - 人格卡与内容同属一个网格行，展开时内容右移，不覆盖卡片
 */
import { computed, nextTick, onBeforeUnmount, onMounted, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import FadeScroll from '../../components/shell/FadeScroll.vue'
import PersonaPicker from '../../components/shell/PersonaPicker.vue'
import { stateCopy } from '../../core/stateCopy'
import RuleCard from './RuleCard.vue'
import RuleDetailPanel from './RuleDetailPanel.vue'
import RuleDetailToolbar from './RuleDetailToolbar.vue'
import RulesToolbar from './RulesToolbar.vue'
import { useRulesView } from './useRulesView'

const {
  personas,
  personaUid,
  personaOpen,
  rules,
  saving,
  pane,
  draftReaction,
  draftAvoid,
  savingDraft,
  saveError,
  discardOpen,
  evidence,
  activePersona,
  activeRule,
  count,
  viewState,
  busy,
  partialError,
  errorReason,
  llmUnavailable,
  draftValidation,
  draftDirty,
  canSaveDraft,
  init,
  choosePersona,
  refresh,
  retry,
  toggleEnabled,
  openDetail,
  requestBack,
  confirmDiscard,
  retryEvidence,
  saveDraft,
  togglePersona,
  goChat,
  bindListScroll,
} = useRulesView()

const emptyCopy = stateCopy('rules.empty.no-data')
const errorCopy = stateCopy('rules.error.load')
const degradedCopy = stateCopy('rules.degraded.llm')

/** 双列模式下规则为单数时，最后一条跨两列；抽屉打开（单列）时不跨 */
const wideLastId = computed<number | null>(() => {
  if (personaOpen.value || rules.value.length % 2 === 0) {
    return null
  }
  return rules.value.at(-1)?.id ?? null
})

function closePersonas(): void {
  if (!personaOpen.value) {
    return
  }
  personaOpen.value = false
  document.getElementById('rules-persona-trigger')?.focus()
}

function onDocumentPointerDown(event: PointerEvent): void {
  const target = event.target
  if (!(target instanceof Element)) {
    return
  }
  if (target.closest('#rules-personas') || target.closest('.toolbar')) {
    return
  }
  personaOpen.value = false
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    closePersonas()
  }
}

watch(personaOpen, (open) => {
  if (open) {
    document.addEventListener('pointerdown', onDocumentPointerDown)
    document.addEventListener('keydown', onDocumentKeydown)
    void nextTick(() => {
      const active = document.querySelector('#rules-personas .persona-row.is-active')
      if (active instanceof HTMLElement) {
        active.focus()
      }
    })
    return
  }
  document.removeEventListener('pointerdown', onDocumentPointerDown)
  document.removeEventListener('keydown', onDocumentKeydown)
})

onMounted(() => {
  void init()
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerDown)
  document.removeEventListener('keydown', onDocumentKeydown)
})
</script>

<template>
  <BaseWorkbench>
    <div
      class="rules"
      :data-view-state="pane === 'list' ? viewState : 'default'"
      :data-degraded="llmUnavailable ? 'llm' : undefined"
      :aria-busy="busy ? 'true' : undefined"
    >
      <template v-if="pane === 'list'">
        <RulesToolbar
          :persona="activePersona"
          :persona-open="personaOpen"
          :count="count"
          :state="viewState"
          :busy="busy"
          @toggle="togglePersona"
          @refresh="refresh"
        />

        <div v-if="partialError" class="error-strip" role="alert">
          <span class="error-strip-text">{{ partialError }}</span>
          <BaseButton variant="ghost" size="sm" @click="retry">重试</BaseButton>
        </div>

        <div v-if="llmUnavailable && viewState !== 'error'" class="degraded-strip" role="status">
          <span class="degraded-title">{{ degradedCopy.title }}</span>
          <span class="degraded-text">{{ degradedCopy.description }}</span>
        </div>

        <div class="rules-split" :class="{ 'is-open': personaOpen }">
          <PersonaPicker
            v-if="personaOpen"
            id="rules-personas"
            :personas="personas"
            :active-uid="personaUid"
            @select="choosePersona"
          />

          <FadeScroll :ref="bindListScroll" class="rules-body">
            <div class="rules-content">
              <div v-if="viewState === 'loading'" class="panel-skeleton">
                <BaseSkeleton v-for="index in 4" :key="index" shape="card" :lines="3" />
              </div>

              <BaseEmptyState
                v-else-if="viewState === 'error'"
                variant="blocked"
                :title="errorCopy.title"
                :description="errorCopy.description"
              >
                <div class="state-slot">
                  <p class="state-reason">{{ errorReason }}</p>
                  <BaseButton variant="secondary" size="sm" @click="retry">重试</BaseButton>
                </div>
              </BaseEmptyState>

              <BaseEmptyState
                v-else-if="viewState === 'empty'"
                variant="guide"
                :title="emptyCopy.title"
                :description="emptyCopy.description"
              >
                <BaseButton variant="ghost" size="sm" @click="goChat">
                  {{ emptyCopy.action }}
                </BaseButton>
              </BaseEmptyState>

              <div v-else class="panel">
                <RuleCard
                  v-for="rule in rules"
                  :key="rule.id"
                  :rule="rule"
                  :class="{ 'is-wide': rule.id === wideLastId }"
                  :saving="saving[rule.id] === true"
                  @open-detail="openDetail(rule)"
                  @toggle="toggleEnabled(rule)"
                />
              </div>
            </div>
          </FadeScroll>
        </div>
      </template>

      <template v-else-if="activeRule">
        <RuleDetailToolbar
          :rule="activeRule"
          :can-save="canSaveDraft"
          :dirty="draftDirty"
          :draft-saving="savingDraft"
          :toggle-saving="saving[activeRule.id] === true"
          @back="requestBack"
          @save="saveDraft"
          @toggle="toggleEnabled(activeRule)"
        />

        <div v-if="llmUnavailable" class="degraded-strip" role="status">
          <span class="degraded-title">{{ degradedCopy.title }}</span>
          <span class="degraded-text">{{ degradedCopy.description }}</span>
        </div>

        <RuleDetailPanel
          :rule="activeRule"
          :evidence="evidence"
          :reaction="draftReaction"
          :avoid="draftAvoid"
          :draft-saving="savingDraft"
          :validation="draftValidation"
          :error="saveError"
          @update:reaction="draftReaction = $event"
          @update:avoid="draftAvoid = $event"
          @retry-evidence="retryEvidence"
        />
      </template>
    </div>
  </BaseWorkbench>

  <BaseModal :open="discardOpen" title="放弃未保存的修改？" @close="discardOpen = false">
    <p class="modal-copy">规则文本或禁忌词还没有保存，返回列表会丢失这次修改。</p>
    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" @click="discardOpen = false">继续编辑</BaseButton>
      <BaseButton @click="confirmDiscard">放弃并返回</BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
/* 主内容区固定 800×600 并居中；窗口不足时压缩高度而不出现横向滚动 */
.rules {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  width: 100%;
  max-width: 800px;
  height: 600px;
  max-height: 100%;
  min-height: 0;
  margin: 0 auto;
}

/* 人格抽屉与列表同排：抽屉占满可用高度，列表在右侧独立滚动 */
.rules-split {
  display: grid;
  flex: 1;
  grid-template-columns: 0 minmax(0, 1fr);
  gap: 0;
  align-items: stretch;
  min-height: 0;
  transition:
    grid-template-columns var(--dur-base) var(--ease-out),
    gap var(--dur-base) var(--ease-out);
}

.rules-split.is-open {
  grid-template-columns: 200px minmax(0, 1fr);
  gap: var(--space-20);
}

.rules-body {
  grid-column: 2;
  min-height: 0;
}

.rules-content {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  min-width: 0;
}

/* 双列栅格：行内卡片等高，过长正文由整行拉伸吸收 */
.panel {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-10);
  align-items: stretch;
  padding-right: var(--space-6);
}

/* 抽屉打开后空间变窄，卡片回到单列 */
.rules-split.is-open .panel,
.rules-split.is-open .panel-skeleton {
  grid-template-columns: minmax(0, 1fr);
}

/* 单数条数：最后一条跨两列，避免右下留空 */
.panel :deep(.rule-card.is-wide) {
  grid-column: 1 / -1;
}

.panel-skeleton {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-10);
  padding-right: var(--space-6);
}

.error-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-inner);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.error-strip-text {
  flex: 1;
  min-width: 0;
}

.degraded-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-inner);
  background: var(--warn-surface);
  color: var(--warn);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.degraded-title {
  font-weight: var(--fw-semibold);
}

.state-slot {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-8);
}

.state-reason {
  max-width: 42ch;
  margin: 0;
  color: var(--danger);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.modal-copy {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-body);
  line-height: 1.7;
}

.spacer {
  flex: 1;
}

/* 窄窗统一单列：人格卡叠在内容上方，卡片单列排布 */
@media (max-width: 860px) {
  .rules {
    height: auto;
    max-height: none;
  }

  .rules-split,
  .rules-split.is-open {
    grid-template-columns: minmax(0, 1fr);
  }

  .rules-split.is-open {
    gap: var(--space-12);
  }

  .rules-body {
    grid-column: 1;
  }
}

@media (max-width: 720px) {
  .panel,
  .panel-skeleton {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
