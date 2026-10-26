<script setup lang="ts">
/**
 * src/views/debug/index.vue — 调试视图入口
 *
 * 职责:
 * - 编排三个页签（关键词池 / 说话风格 / 评估产物）与加载 / 错误 / 空 / 默认四态
 * - 把页签与路由参数双向同步，刷新与深链都停在同一页签
 *
 * 设计特点:
 * - 内容区固定 800×600 并居中，超出部分在容器内滚动并在边缘渐隐
 * - 工具栏承载页签与当前页签动作，页签切换只改路由
 * - 评估目录不可读走视图内降级条，不替换内容，也不影响其余页签
 * - 天然只读：不出现只读横幅，也没有会话级的受限态
 */

import { computed, onMounted, watch } from 'vue'
import { useRoute } from 'vue-router'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import FadeScroll from '../../components/shell/FadeScroll.vue'
import { stateCopy } from '../../core/stateCopy'
import DebugToolbar from './DebugToolbar.vue'
import EvalPanel from './EvalPanel.vue'
import KeywordPanel from './KeywordPanel.vue'
import StylePanel from './StylePanel.vue'
import { useDebugView } from './useDebugView'

const route = useRoute()

const {
  tab,
  tabs,
  viewState,
  busy,
  errorReason,
  partialError,
  degradedDir,
  syncFromRoute,
  selectTab,
  ensureCurrent,
  refresh,
  retry,
  pool,
  pendingAliases,
  resolving,
  resolveAlias,
  personas,
  personaUid,
  styleModel,
  selectStylePersona,
  evalDir,
  evalFiles,
  artifact,
  pickEvalDir,
  openArtifact,
  closeArtifact,
} = useDebugView()

const errorCopy = stateCopy('debug.error.load')
const degradedCopy = stateCopy('debug.degraded.dir')

/** 空态文案按页签归因：产物页再按「是否已选目录」拆成因，另两页各有自己的说明 */
const emptyCopy = computed(() => {
  if (tab.value === 'keywords') {
    return stateCopy('debug.empty.keywords')
  }
  if (tab.value === 'style') {
    return stateCopy('debug.empty.style')
  }
  return stateCopy(evalDir.value === '' ? 'debug.empty.no-data' : 'debug.empty.eval-dir')
})

/** 空态动作：只有评估页给「选择目录」 */
const emptyAction = computed(() => (tab.value === 'eval' ? emptyCopy.value.action ?? '' : ''))

function onEmptyAction(): void {
  if (tab.value === 'eval') {
    void pickEvalDir()
  }
}

function onRefresh(): void {
  void refresh()
}

onMounted(() => {
  syncFromRoute()
  void ensureCurrent()
})

watch(
  () => route.params.tab,
  () => {
    syncFromRoute()
    void ensureCurrent()
  },
)
</script>

<template>
  <BaseWorkbench>
    <div
      class="debug"
      :data-view-state="viewState"
      :data-degraded="degradedDir ? 'dir' : undefined"
      :aria-busy="busy ? 'true' : undefined"
    >
      <DebugToolbar :tab="tab" :tabs="tabs" @select="selectTab">
        <template #actions>
          <BaseButton
            v-if="tab === 'eval'"
            variant="secondary"
            size="sm"
            @click="pickEvalDir"
          >
            选择目录
          </BaseButton>
          <BaseButton
            variant="ghost"
            size="sm"
            icon-only
            :loading="busy"
            aria-label="刷新当前页签"
            @click="onRefresh"
          >
            <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
              <polyline points="23 4 23 10 17 10" />
              <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
            </svg>
          </BaseButton>
        </template>
      </DebugToolbar>

      <div v-if="degradedDir && viewState !== 'error'" class="degraded-strip" role="status">
        <span class="degraded-title">{{ degradedCopy.title }}</span>
        <span class="degraded-text">{{ degradedCopy.description }}</span>
        <span class="degraded-reason">{{ degradedDir }}</span>
        <BaseButton variant="ghost" size="sm" @click="pickEvalDir">
          {{ degradedCopy.action }}
        </BaseButton>
      </div>

      <div v-if="partialError && viewState !== 'error'" class="error-strip" role="alert">
        <span class="error-strip-text">{{ partialError }}</span>
        <BaseButton variant="ghost" size="sm" @click="retry">
          {{ errorCopy.action }}
        </BaseButton>
      </div>

      <FadeScroll class="debug-body">
        <div class="debug-content">
          <div v-if="viewState === 'loading'" class="skeletons">
            <BaseSkeleton v-if="tab === 'eval'" shape="table" :lines="6" />
            <template v-else>
              <BaseSkeleton v-for="index in 3" :key="index" shape="card" :lines="3" />
            </template>
          </div>

          <BaseEmptyState
            v-else-if="viewState === 'error'"
            variant="blocked"
            :title="errorCopy.title"
            :description="errorCopy.description"
          >
            <div class="state-slot">
              <p class="state-reason">{{ errorReason }}</p>
              <BaseButton variant="secondary" size="sm" @click="retry">
                {{ errorCopy.action }}
              </BaseButton>
            </div>
          </BaseEmptyState>

          <BaseEmptyState
            v-else-if="viewState === 'empty'"
            variant="action"
            :title="emptyCopy.title"
            :description="emptyCopy.description"
          >
            <BaseButton v-if="emptyAction" variant="secondary" size="sm" @click="onEmptyAction">
              {{ emptyAction }}
            </BaseButton>
          </BaseEmptyState>

          <KeywordPanel
            v-else-if="tab === 'keywords'"
            :pool="pool"
            :pending="pendingAliases"
            :resolving="resolving"
            @resolve="resolveAlias"
          />

          <StylePanel
            v-else-if="tab === 'style'"
            :personas="personas"
            :persona-uid="personaUid"
            :model="styleModel"
            @select-persona="selectStylePersona"
          />

          <EvalPanel
            v-else
            :dir="evalDir"
            :files="evalFiles"
            :artifact="artifact"
            @open="openArtifact"
            @close="closeArtifact"
          />
        </div>
      </FadeScroll>
    </div>
  </BaseWorkbench>
</template>

<style scoped>
.debug {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  width: 100%;
  max-width: 800px;
  height: 600px;
  max-height: 100%;
  min-height: 0;
  margin: 0 auto;
}

.debug-body {
  flex: 1;
  min-height: 0;
}

.debug-content {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding-right: var(--space-6);
}

.skeletons {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
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

.degraded-reason {
  flex: 1;
  min-width: 0;
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  overflow-wrap: anywhere;
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

.icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

@media (max-width: 860px) {
  .debug {
    height: auto;
    max-height: none;
  }
}
</style>
