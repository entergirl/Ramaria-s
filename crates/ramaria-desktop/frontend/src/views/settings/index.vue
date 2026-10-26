<script setup lang="ts">
/**
 * src/views/settings/index.vue — 设置视图入口
 *
 * 职责:
 * - 编排两级目录、卡片栅格与加载 / 错误 / 默认三态
 * - 把深链（`#/settings/:scope/:section`）与页面目录双向同步
 *
 * 设计特点:
 * - 路由是目录的唯一来源；点击目录只改路由，由路由回写 store，避免两套状态分叉
 * - 进阶未解锁时不出现在范围切换里，深链进入也会被收敛回基础页
 * - 卡片各自保存与报错；主请求失败才整页错误
 * - 12 列栅格跨度走静态 class（生产源码禁止内联样式）
 */

import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import FadeScroll from '../../components/shell/FadeScroll.vue'
import {
  type SettingsScope,
  defaultSectionId,
  findSection,
  isGroupCard,
  isSettingsScope,
} from '../../config/settingsSections'
import type { SettingField } from '../../config/schema/define'
import type { JsonValue } from '../../config/configTypes'
import { groupById } from '../../config/settingsSchema'
import { stateCopy } from '../../core/stateCopy'
import { useAppStore } from '../../stores/app'
import { useDebugStore } from '../../stores/debug'
import { useSettingsStore } from '../../stores/settings'
import SettingsToolbar from './SettingsToolbar.vue'
import SchemaGroupCard from './SchemaGroupCard.vue'
import BackendCard from './custom/BackendCard.vue'
import ChannelCard from './custom/ChannelCard.vue'
import DataCard from './custom/DataCard.vue'
import DiagnosticsCard from './custom/DiagnosticsCard.vue'
import EmbeddingCard from './custom/EmbeddingCard.vue'
import McpCard from './custom/McpCard.vue'
import PrivacyCard from './custom/PrivacyCard.vue'
import ProactiveOverviewCard from './custom/ProactiveOverviewCard.vue'
import ProactiveRosterCard from './custom/ProactiveRosterCard.vue'
import { useSettingsView } from './useSettingsView'

const route = useRoute()
const router = useRouter()
const app = useAppStore()
const debug = useDebugStore()
const settingsStore = useSettingsStore()

const {
  draft,
  defaults,
  defaultsError,
  viewState,
  loadError,
  loading,
  section,
  scope,
  sectionId,
  sections,
  dirtyBySection,
  savingCard,
  cardErrors,
  isGroupDirty,
  setField,
  saveGroup,
  resetGroup,
  applyRoute,
  load,
} = useSettingsView()

const advancedUnlocked = computed(() => debug.unlocked)

const errorCopy = stateCopy('settings.error.load')

/** 面板滚动实例：切换范围 / 分类时回到顶部，避免沿用上一页的滚动位置 */
const settingsScroll = ref<{ scrollTo: (top: number) => void } | null>(null)

watch([scope, sectionId], () => {
  void nextTick(() => settingsScroll.value?.scrollTo(0))
})

/** 当前子页的卡片：键表驱动的卡带上组声明 */
const cards = computed(() =>
  section.value.cards.map((card) => ({
    ...card,
    group: isGroupCard(card.kind) ? groupById(card.id) : undefined,
  })),
)

/** 降级来源（卡内标记的机器口径，供 QA 与状态画廊取数） */
const degradedKind = computed(() => {
  if (app.appState === 'degraded') {
    if (app.degradedReason === 'embedding_missing') {
      return 'embedding'
    }
    if (app.degradedReason === 'llm_unavailable') {
      return 'llm'
    }
    if (app.degradedReason === 'both_unavailable') {
      return 'both'
    }
    return 'degraded'
  }
  if (app.indexNotReady) {
    return 'index'
  }
  if (app.indexPartial) {
    return 'partial'
  }
  return undefined
})

function routeParam(name: 'scope' | 'section'): string {
  const value = route.params[name]
  return Array.isArray(value) ? (value[0] ?? '') : String(value ?? '')
}

/** 路由 → store：收敛非法范围 / 未知子页 / 未解锁的进阶深链 */
function syncFromRoute(): void {
  const rawScope = routeParam('scope')
  const requested: SettingsScope = isSettingsScope(rawScope) ? rawScope : 'basic'
  const nextScope: SettingsScope =
    requested === 'advanced' && !advancedUnlocked.value ? 'basic' : requested
  const remembered = settingsStore.lastSection[nextScope]
  const rawSection = routeParam('section')
  const nextSection =
    findSection(nextScope, rawSection)?.id ??
    findSection(nextScope, remembered)?.id ??
    defaultSectionId(nextScope)

  if (rawScope !== nextScope || rawSection !== nextSection) {
    void router.replace({ name: 'settings', params: { scope: nextScope, section: nextSection } })
  }
  applyRoute(nextScope, nextSection)
}

function selectScope(nextScope: SettingsScope): void {
  if (nextScope === 'advanced' && !advancedUnlocked.value) {
    return
  }
  const remembered = settingsStore.lastSection[nextScope]
  const nextSection = findSection(nextScope, remembered)?.id ?? defaultSectionId(nextScope)
  void router.replace({ name: 'settings', params: { scope: nextScope, section: nextSection } })
}

function selectSection(nextSection: string): void {
  void router.replace({
    name: 'settings',
    params: { scope: settingsStore.scope, section: nextSection },
  })
}

function onFieldUpdate(field: SettingField, value: JsonValue): void {
  setField(field, value)
}

function retryLoad(): void {
  void load(true)
}

onMounted(() => {
  syncFromRoute()
  void load()
})

watch(
  () => [routeParam('scope'), routeParam('section')],
  () => {
    syncFromRoute()
  },
)

watch(advancedUnlocked, (unlocked) => {
  if (!unlocked && settingsStore.scope === 'advanced') {
    void router.replace({
      name: 'settings',
      params: { scope: 'basic', section: settingsStore.lastSection.basic },
    })
  }
})
</script>

<template>
  <BaseWorkbench>
    <div
      class="settings"
      :data-view-state="viewState"
      :data-degraded="degradedKind"
      :aria-busy="loading ? 'true' : undefined"
    >
      <SettingsToolbar
        :scope="scope"
        :section-id="sectionId"
        :sections="sections"
        :dirty-counts="dirtyBySection"
        :advanced-unlocked="advancedUnlocked"
        @select-scope="selectScope"
        @select-section="selectSection"
      />

      <FadeScroll ref="settingsScroll" class="settings-body">
        <div v-if="viewState === 'loading'" class="settings-grid">
          <div class="s-grid-item span-6">
            <BaseSkeleton shape="card" :lines="4" />
          </div>
          <div class="s-grid-item span-6">
            <BaseSkeleton shape="card" :lines="4" />
          </div>
        </div>

        <BaseEmptyState
          v-else-if="viewState === 'error'"
          variant="blocked"
          :title="errorCopy.title"
          :description="errorCopy.description"
        >
          <div class="state-slot">
            <p class="state-reason">{{ loadError }}</p>
            <BaseButton variant="secondary" size="sm" @click="retryLoad">重试</BaseButton>
          </div>
        </BaseEmptyState>

        <div v-else class="settings-grid">
          <div v-for="card in cards" :key="card.id" class="s-grid-item" :class="`span-${card.span}`">
            <SchemaGroupCard
              v-if="card.kind === 'schema' && card.group && draft"
              :data-card="card.id"
              :group="card.group"
              :document="draft"
              :dirty="isGroupDirty(card.id)"
              :saving="savingCard === card.id"
              :error="cardErrors[card.id] ?? ''"
              :reset-disabled="!defaults"
              :reset-hint="defaultsError"
              @update="onFieldUpdate"
              @save="saveGroup(card.id)"
              @reset="resetGroup(card.id)"
            />

            <BackendCard
              v-else-if="card.kind === 'backend' && card.group && draft"
              :data-card="card.id"
              :group="card.group"
              :document="draft"
              :dirty="isGroupDirty(card.id)"
              :saving="savingCard === card.id"
              :error="cardErrors[card.id] ?? ''"
              :reset-disabled="!defaults"
              :reset-hint="defaultsError"
              :save-group="() => saveGroup(card.id)"
              @update="onFieldUpdate"
              @reset="resetGroup(card.id)"
            />

            <EmbeddingCard
              v-else-if="card.kind === 'embedding' && card.group && draft"
              :data-card="card.id"
              :group="card.group"
              :document="draft"
              :dirty="isGroupDirty(card.id)"
              :saving="savingCard === card.id"
              :error="cardErrors[card.id] ?? ''"
              :reset-disabled="!defaults"
              :reset-hint="defaultsError"
              :save-group="() => saveGroup(card.id)"
              @update="onFieldUpdate"
              @reset="resetGroup(card.id)"
            />

            <ChannelCard
              v-else-if="card.kind === 'channel' && card.group && draft"
              :data-card="card.id"
              :group="card.group"
              :document="draft"
              :dirty="isGroupDirty(card.id)"
              :saving="savingCard === card.id"
              :error="cardErrors[card.id] ?? ''"
              :reset-disabled="!defaults"
              :reset-hint="defaultsError"
              @update="onFieldUpdate"
              @save="saveGroup(card.id)"
              @reset="resetGroup(card.id)"
            />

            <McpCard
              v-else-if="card.kind === 'mcp' && card.group && draft"
              :data-card="card.id"
              :group="card.group"
              :document="draft"
              :dirty="isGroupDirty(card.id)"
              :saving="savingCard === card.id"
              :error="cardErrors[card.id] ?? ''"
              :reset-disabled="!defaults"
              :reset-hint="defaultsError"
              @update="onFieldUpdate"
              @save="saveGroup(card.id)"
              @reset="resetGroup(card.id)"
            />

            <PrivacyCard v-else-if="card.kind === 'privacy'" />

            <ProactiveOverviewCard v-else-if="card.kind === 'proactive-overview'" />

            <ProactiveRosterCard v-else-if="card.kind === 'proactive-roster'" />

            <DataCard v-else-if="card.kind === 'data'" />

            <DiagnosticsCard v-else-if="card.kind === 'diagnostics'" />
          </div>
        </div>
      </FadeScroll>
    </div>
  </BaseWorkbench>
</template>

<style scoped>
.settings {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  width: 100%;
  max-width: var(--w-workbench);
  height: 640px;
  max-height: calc(100vh - var(--space-40) * 2);
  min-height: 0;
  margin: 0 auto;
}

.settings-body {
  flex: 1;
  min-height: 0;
}

/* 12 列栅格：跨度走静态 class，行内等高让每页收成完整矩形 */
.settings-grid {
  display: grid;
  grid-template-columns: repeat(12, minmax(0, 1fr));
  gap: var(--gap-card);
  align-items: stretch;
  padding-right: var(--space-6);
}

/* 卡片撑满所在栅格单元，页脚由卡内 margin-top:auto 贴底 */
.s-grid-item {
  display: flex;
  min-width: 0;
}

.s-grid-item > * {
  flex: 1;
  min-width: 0;
}

.span-4 {
  grid-column: span 4;
}

.span-5 {
  grid-column: span 5;
}

.span-6 {
  grid-column: span 6;
}

.span-7 {
  grid-column: span 7;
}

.span-8 {
  grid-column: span 8;
}

.span-12 {
  grid-column: span 12;
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

/* ≤860：面板仍用内层滚动，只收掉滚动条预留的右侧空隙 */
@media (max-width: 860px) {
  .settings-grid {
    padding-right: 0;
  }
}

/* ≤720：12 列降为单列 */
@media (max-width: 720px) {
  .settings-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .span-4,
  .span-5,
  .span-6,
  .span-7,
  .span-8,
  .span-12 {
    grid-column: auto;
  }
}
</style>
