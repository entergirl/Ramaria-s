<script setup lang="ts">
/**
 * src/views/memory/index.vue — 记忆视图入口
 *
 * 职责:
 * - 编排工具栏、人格卡、四页签与各层记忆卡片
 * - 派生视图主态（加载 / 错误 / 空 / 默认）与忙碌标记
 *
 * 设计特点:
 * - 数据与动作在 composable 与 store 中，本组件只做布局与事件接线
 * - 页签走共用的胶囊分类键条（浅底背板 + 胶囊键，选中深底浅字）
 * - 页内横幅（刷新进度 / 索引进度 / 部分失败）占位在工具栏与页签之间，内容态让到其下
 * - 人格卡与内容同属一个网格行，展开时内容整体右移，不覆盖卡片
 * - 详情在卡内展开且同时只开一张；切页签自动收起
 * - 页面定高 800×600：工具栏、横幅与页签固定，卡片区内部滚动并在上下边缘渐隐
 */
import { computed, nextTick, onBeforeUnmount, onMounted, watch } from 'vue'
import { useRouter } from 'vue-router'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseProgress from '../../components/base/BaseProgress.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseChipNav from '../../components/base/BaseChipNav.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import FadeScroll from '../../components/shell/FadeScroll.vue'
import PersonaPicker from '../../components/shell/PersonaPicker.vue'
import { parseL1Context } from '../../core/memory/context'
import { displayPersonaName, orderFactGroups } from '../../core/memory/format'
import { stateCopy } from '../../core/stateCopy'
import MemoryKnowledgeCard from './MemoryKnowledgeCard.vue'
import MemoryL1Card from './MemoryL1Card.vue'
import MemoryL2Card from './MemoryL2Card.vue'
import MemoryL3Panel from './MemoryL3Panel.vue'
import MemoryToolbar from './MemoryToolbar.vue'
import { useMemoryView } from './useMemoryView'

const router = useRouter()
const {
  store,
  personaOpen,
  detailId,
  excerpts,
  evidence,
  processing,
  reloadingProfile,
  tabOptions,
  viewState,
  busy,
  canDeepProcess,
  partialError,
  degraded,
  indexBanner,
  hasBanner,
  bindWorkbench,
  init,
  choosePersona,
  switchTab,
  toggleDetail,
  toggleTrait,
  loadExcerpt,
  loadEvidence,
  openChat,
  openSettings,
  deepProcess,
  reloadProfile,
  retry,
} = useMemoryView()

const emptyCopy = stateCopy('memory.empty.no-data')
const errorCopy = stateCopy('memory.error.load')

const persona = computed(() => store.personas.find((item) => item.uid === store.personaUid) ?? null)

/** 知识分组（固定字段顺序，避免后端哈希顺序抖动） */
const factGroups = computed(() =>
  store.data?.facts ? orderFactGroups(store.data.facts.grouped) : [],
)

/** L1 参与者显示名：后端存人格 UID，界面按人格列表映射 */
function participantsOf(contextJson: string | null): string[] {
  const context = parseL1Context(contextJson)
  return context.partners.map((uid) => displayPersonaName(uid, store.personas))
}

function messageCountOf(contextJson: string | null): number | null {
  return parseL1Context(contextJson).messageCount
}

function personaNameOf(uid: string | null): string {
  return uid ? displayPersonaName(uid, store.personas) : '未归属人格'
}

function goImport(): void {
  void router.push({ name: 'import' })
}

function togglePersonas(): void {
  personaOpen.value = !personaOpen.value
}

function closePersonas(): void {
  if (!personaOpen.value) {
    return
  }
  personaOpen.value = false
  document.getElementById('memory-persona-trigger')?.focus()
}

function onDocumentPointerDown(event: PointerEvent): void {
  const target = event.target
  if (!(target instanceof Element)) {
    return
  }
  if (target.closest('#memory-personas') || target.closest('.toolbar')) {
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
      const active = document.querySelector('#memory-personas .persona-row.is-active')
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
      class="memory"
      :data-view-state="viewState"
      :data-degraded="degraded"
      :aria-busy="busy ? 'true' : undefined"
    >
      <MemoryToolbar
        :persona="persona"
        :open="personaOpen"
        :can-deep-process="canDeepProcess"
        :processing="processing"
        @toggle="togglePersonas"
        @deep-process="deepProcess"
      />

      <div class="memory-split" :class="{ 'is-open': personaOpen }">
        <PersonaPicker
          v-if="personaOpen"
          id="memory-personas"
          :personas="store.personas"
          :active-uid="store.personaUid"
          @select="choosePersona"
        />

        <div class="memory-content">
          <div v-if="hasBanner" class="memory-banners">
            <BaseProgress v-if="store.refreshing" label="刷新记忆…" />

            <div v-if="indexBanner" class="index-strip" :data-kind="indexBanner.kind" role="status">
              <span class="index-strip-title">{{ indexBanner.title }}</span>
              <span class="index-strip-text">{{ indexBanner.description }}</span>
              <BaseButton variant="ghost" size="sm" @click="openSettings">
                {{ indexBanner.action }}
              </BaseButton>
              <progress
                v-if="indexBanner.kind === 'building'"
                class="index-strip-progress"
                :value="indexBanner.percent"
                max="100"
              />
            </div>

            <div v-if="partialError" class="error-strip" role="alert">
              <span class="error-strip-text">{{ partialError }}</span>
              <BaseButton variant="ghost" size="sm" @click="retry">重试</BaseButton>
            </div>
          </div>

          <div class="tabs">
            <BaseChipNav
              :model-value="store.tab"
              :options="tabOptions"
              label="记忆层级"
              @update:model-value="switchTab"
            />
          </div>

          <div v-if="viewState === 'loading'" class="memory-state">
            <div class="skeletons">
              <BaseSkeleton v-for="index in 3" :key="index" shape="card" :lines="3" />
            </div>
          </div>

          <BaseEmptyState
            v-else-if="viewState === 'error'"
            class="memory-state"
            variant="blocked"
            :title="errorCopy.title"
            :description="errorCopy.description"
          >
            <div class="error-slot">
              <p class="error-reason">{{ store.error || store.personasError }}</p>
              <BaseButton variant="secondary" size="sm" @click="retry">重试</BaseButton>
            </div>
          </BaseEmptyState>

          <BaseEmptyState
            v-else-if="viewState === 'empty'"
            class="memory-state"
            variant="action"
            :title="emptyCopy.title"
            :description="emptyCopy.description"
          >
            <BaseButton variant="secondary" size="sm" @click="goImport">
              {{ emptyCopy.action }}
            </BaseButton>
          </BaseEmptyState>

          <FadeScroll v-else-if="store.data" :ref="bindWorkbench" class="memory-scroll">
            <div v-if="store.tab === 'l1'" class="panel">
              <MemoryL1Card
                v-for="item in store.data.l1"
                :key="item.id"
                :item="item"
                :open="detailId === `l1:${item.id}`"
                :participants="participantsOf(item.context_json)"
                :message-count="messageCountOf(item.context_json)"
                :excerpt="excerpts[item.session_id]"
                :persona-name="personaNameOf(item.persona_uid)"
                @toggle="toggleDetail(`l1:${item.id}`, item.session_id)"
                @open-chat="openChat(item.session_id, item.persona_uid)"
                @jump-l2="switchTab('l2')"
                @retry-excerpt="loadExcerpt(item.session_id)"
              />
            </div>

            <div v-else-if="store.tab === 'l2'" class="panel">
              <MemoryL2Card
                v-for="item in store.data.l2"
                :key="item.id"
                :item="item"
                :open="detailId === `l2:${item.id}`"
                :persona-name="personaNameOf(item.persona_uid)"
                @toggle="toggleDetail(`l2:${item.id}`)"
              />
            </div>

            <MemoryL3Panel
              v-else-if="store.tab === 'l3'"
              :profile="store.data.profile"
              :status="store.data.status"
              :detail-id="detailId"
              :evidence="evidence"
              :reloading="reloadingProfile"
              @toggle-trait="toggleTrait"
              @retry-evidence="loadEvidence"
              @reload="reloadProfile"
            />

            <div v-else class="panel">
              <section v-for="group in factGroups" :key="group.field" class="fact-group">
                <header class="fact-group-head">
                  <span class="fact-group-title">{{ group.field }}</span>
                  <span class="fact-group-count">{{ group.facts.length }} 条</span>
                </header>
                <MemoryKnowledgeCard
                  v-for="fact in group.facts"
                  :key="fact.id"
                  :fact="fact"
                  :versions="store.data.facts?.versions[String(fact.id)] ?? []"
                  :open="detailId === `k:${fact.id}`"
                  @toggle="toggleDetail(`k:${fact.id}`)"
                />
              </section>
            </div>
          </FadeScroll>
        </div>
      </div>
    </div>
  </BaseWorkbench>
</template>

<style scoped>
/* 定高工作台：内容列内部滚动并渐隐，页面本身不参与外部滚动 */
.memory {
  display: flex;
  flex-direction: column;
  gap: var(--gap-card);
  width: 100%;
  max-width: 800px;
  height: 600px;
  max-height: calc(100vh - var(--space-40) * 2);
  min-height: 0;
  margin: 0 auto;
}

.memory-split {
  display: grid;
  flex: 1;
  grid-template-columns: 0 minmax(0, 1fr);
  /* 显式单行定高：行高跟容器走，内容再长也不把行撑出定高框 */
  grid-template-rows: minmax(0, 1fr);
  gap: 0;
  align-items: stretch;
  min-height: 0;
  transition:
    grid-template-columns var(--dur-base) var(--ease-out),
    gap var(--dur-base) var(--ease-out);
}

/* 人格卡与内容列都在同一行高内自滚，不被内容撑破 */
.memory-split > * {
  min-height: 0;
}

.memory-split.is-open {
  grid-template-columns: 200px minmax(0, 1fr);
  gap: var(--space-20);
}

.memory-content {
  display: flex;
  grid-column: 2;
  flex-direction: column;
  gap: var(--gap-card);
  height: 100%;
  min-width: 0;
  min-height: 0;
}

/* 卡片区：定高工作台内的唯一滚动容器，上下边缘渐隐 */
.memory-scroll {
  flex: 1;
  min-height: 0;
}

/* 加载 / 空 / 错误态同样收在定高区内，不把页面撑高 */
.memory-state {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}

.tabs {
  display: flex;
  flex: none;
  justify-content: center;
}

.memory-split.is-open .tabs {
  justify-content: flex-start;
}

.panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
}

.skeletons {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding: var(--pad-card-lg);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
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

/* 页内横幅槽位：工具栏与页签键之间，空时不占位 */
.memory-banners {
  display: flex;
  flex: none;
  flex-direction: column;
  gap: var(--space-8);
}

/* 索引横幅：构建中走辅色蓝并在底边画进度，未就绪走警示黄 */
.index-strip {
  position: relative;
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
  overflow: hidden;
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-inner);
  background: var(--warn-surface);
  color: var(--warn);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.index-strip[data-kind='building'] {
  border-color: var(--accent-line);
  background: var(--accent-surface);
  color: var(--accent-text);
}

.index-strip-title {
  font-weight: var(--fw-semibold);
}

.index-strip-text {
  flex: 1;
  min-width: 0;
}

.index-strip[data-kind='building'] .index-strip-text {
  color: var(--text-2);
}

.index-strip-progress {
  position: absolute;
  inset: auto 0 0 0;
  width: 100%;
  height: 2px;
  border: none;
  appearance: none;
}

.index-strip-progress::-webkit-progress-bar {
  background: transparent;
}

.index-strip-progress::-webkit-progress-value {
  background: var(--accent);
}

.error-slot {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-8);
}

.error-reason {
  max-width: 42ch;
  color: var(--danger);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.fact-group {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.fact-group-head {
  display: flex;
  align-items: baseline;
  gap: var(--space-8);
  padding: 0 var(--space-4);
}

.fact-group-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.fact-group-count {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

/* 窄窗统一单列：收起时也不保留 0 宽的首列，人格卡展开后叠在内容上方 */
@media (max-width: 860px) {
  .memory-split,
  .memory-split.is-open {
    grid-template-columns: minmax(0, 1fr);
  }

  /* 窄窗改为上下两段：人格卡占约 42%，内容占其余高度，两者各自滚动 */
  .memory-split {
    grid-template-rows: minmax(0, 1fr);
  }

  .memory-split.is-open {
    grid-template-rows: minmax(0, 0.42fr) minmax(0, 1fr);
    gap: var(--space-12);
  }

  .memory-content {
    grid-column: 1;
  }

  /* 人格卡叠在内容上方时页签仍居中，不跟随左对齐 */
  .memory-split.is-open .tabs {
    justify-content: center;
  }
}
</style>
