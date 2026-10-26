<script setup lang="ts">
/**
 * src/views/dev/gallery/ViewStateThumb.vue — 视图状态缩略
 *
 * 职责:
 * - 用基础组件复现视图六态的载体：内容版式、骨架、空态、错误块、降级条与受限条
 *
 * 设计特点:
 * - 只表达载体，不伪造视图数据；文案优先取状态文案表，未登记的给载体级说明
 * - 降级条复用对话横幅，卡内标记用标签表达，与视图口径一致
 * - 版式按视图分组示意，便于并排比较密度而非具体内容
 */
import { computed, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseEmptyState from '../../../components/base/BaseEmptyState.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import BaseProgress from '../../../components/base/BaseProgress.vue'
import BaseSkeleton from '../../../components/base/BaseSkeleton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import ChatBanner from '../../chat/ChatBanner.vue'
import type { ViewCell } from './catalog'
import { viewThumbCopy, viewThumbSpec } from './viewCells'

const props = defineProps<{
  view: string
  cell: ViewCell
}>()

const spec = computed(() => viewThumbSpec(props.view))
const copy = computed(() => viewThumbCopy(props.view, props.cell))

const lockedText = ref('')

const loadingLabel = computed(() => {
  if (props.cell.variant === 'analyze') {
    return '分析中'
  }
  if (props.cell.variant === 'execute') {
    return '执行中'
  }
  return copy.value.title
})

const loadingValue = computed(() => {
  if (props.cell.variant === 'analyze') {
    return 18
  }
  if (props.cell.variant === 'execute') {
    return 62
  }
  return undefined
})

const retrying = computed(() => props.cell.variant === 'retrying')
const cardError = computed(() => props.cell.variant === 'card')
</script>

<template>
  <div class="thumb" :data-thumb="cell.carrier" :data-variant="cell.variant">
    <!-- 默认：各视图的内容版式示意 -->
    <template v-if="cell.carrier === 'default'">
      <div v-if="view === 'chat'" class="mock-chat">
        <p class="mock-divider">会话开始 · 21:04</p>
        <p class="mock-bubble is-left">那就先歇一会儿，别硬撑。</p>
        <p class="mock-bubble is-right">今天有点累。</p>
        <p class="mock-composer">说点什么…</p>
      </div>

      <div v-else-if="view === 'setup'" class="mock-form">
        <p class="mock-step">第 1 / 3 步 · 对话模型</p>
        <p class="mock-field">推理服务地址</p>
        <p class="mock-field">模型名称</p>
        <div class="mock-actions">
          <span class="mock-btn is-primary">测试连接</span>
          <span class="mock-btn">下一步</span>
        </div>
      </div>

      <div v-else-if="view === 'memory'" class="mock-stack">
        <div class="mock-tabs">
          <span class="is-on">摘要</span>
          <span>事件</span>
          <span>画像</span>
          <span>知识</span>
        </div>
        <p class="mock-line is-title">周三的搬家</p>
        <p class="mock-line">情绪从紧张转向期待，提到想给阳台添一盆植物。</p>
      </div>

      <div v-else-if="view === 'persona'" class="mock-grid">
        <div v-for="name in ['Rama', '小珊', '我']" :key="name" class="mock-persona">
          <span class="mock-avatar">{{ name.slice(0, 1) }}</span>
          <span class="mock-line">{{ name }}</span>
        </div>
      </div>

      <div v-else-if="view === 'rules'" class="mock-stack">
        <p class="mock-line is-title">说话直接一点</p>
        <p class="mock-line">仅参数注入 · 生效中 · 置信度 0.72</p>
      </div>

      <div v-else-if="view === 'import'" class="mock-form">
        <p class="mock-step">第 2 / 4 步 · 预览</p>
        <p class="mock-drop">拖入导出文件，或选择文件</p>
        <div class="mock-actions">
          <span class="mock-btn is-primary">开始导入</span>
        </div>
      </div>

      <div v-else-if="view === 'settings'" class="mock-settings">
        <div class="mock-nav">
          <span class="is-on">模型与检索</span>
          <span>会话与隐私</span>
          <span>主动对话</span>
        </div>
        <div class="mock-stack">
          <p class="mock-line is-title">后端配置</p>
          <p class="mock-line">本地推理 · qwen3-8b</p>
        </div>
      </div>

      <div v-else class="mock-stack">
        <p class="mock-line is-title">关键词池</p>
        <p class="mock-line">搬家 · 12 次 · 权重 0.82</p>
        <p class="mock-line">阳台 · 6 次 · 权重 0.41</p>
      </div>
    </template>

    <!-- 加载：全屏页用按钮级指示，其余用顶部进度加骨架 -->
    <template v-else-if="cell.carrier === 'loading'">
      <div v-if="view === 'setup'" class="mock-form">
        <p class="mock-step">第 2 / 3 步 · 嵌入模型</p>
        <p class="mock-field">模型目录</p>
        <BaseButton size="sm" variant="primary" loading>校验模型</BaseButton>
      </div>
      <template v-else>
        <BaseProgress :label="loadingLabel" :value="loadingValue" />
        <BaseSkeleton :shape="spec.skeleton" :lines="2" />
      </template>
    </template>

    <!-- 空：三变体空态 -->
    <BaseEmptyState
      v-else-if="cell.carrier === 'empty'"
      :variant="spec.emptyVariant"
      :title="copy.title"
      :description="copy.description"
    >
      <BaseButton v-if="copy.action" size="sm" variant="secondary">{{ copy.action }}</BaseButton>
    </BaseEmptyState>

    <!-- 错误：卡内错误条或视图错误块 -->
    <template v-else-if="cell.carrier === 'error'">
      <BaseCard v-if="cardError" state="error" error-text="嵌入模型校验失败，其余设置不受影响">
        <p class="mock-line">嵌入与视觉</p>
      </BaseCard>
      <div v-else class="mock-error" role="alert">
        <p class="mock-error-title">{{ copy.title }}</p>
        <p class="mock-line">{{ copy.description }}</p>
        <BaseButton v-if="copy.action" size="sm" variant="secondary" :loading="retrying">
          {{ copy.action }}
        </BaseButton>
      </div>
    </template>

    <!-- 降级：视图条或卡内标记 -->
    <template v-else-if="cell.carrier === 'degraded'">
      <ChatBanner
        v-if="cell.banner"
        :kind="cell.banner"
        :title="copy.title"
        :description="copy.description"
        :action="copy.action ?? ''"
        :percent="cell.percent ?? 0"
      />
      <div v-else class="mock-mark">
        <BaseTag tone="warn">{{ copy.title }}</BaseTag>
        <p class="mock-line">{{ copy.description }}</p>
        <BaseButton v-if="copy.action" size="sm" variant="ghost">{{ copy.action }}</BaseButton>
      </div>
    </template>

    <!-- 受限：只读条或锁定控件 -->
    <template v-else>
      <div v-if="cell.variant === 'readonly'" class="mock-readonly">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <rect x="4" y="11" width="16" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 0 1 8 0v4" />
        </svg>
        <span>{{ copy.description }}</span>
        <BaseButton v-if="copy.action" size="sm" variant="ghost">{{ copy.action }}</BaseButton>
      </div>
      <div v-else class="mock-locked">
        <BaseInput v-model="lockedText" label="输入区" placeholder="正在生成…" disabled />
        <p class="mock-line">{{ copy.description }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.thumb {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  width: 100%;
  min-width: 0;
}

/* ---- 默认版式示意 ---- */

.mock-chat {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.mock-divider {
  align-self: center;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.mock-bubble {
  max-width: 80%;
  padding: var(--space-6) var(--space-10);
  border-radius: var(--radius-bubble);
  background: var(--bg-card);
  color: var(--text-2);
  font-size: var(--fs-small);
  box-shadow: var(--shadow-sm);
}

.mock-bubble.is-right {
  align-self: flex-end;
  border-color: var(--bubble-user-line);
  background: var(--bubble-user-bg);
  color: var(--text-1);
}

.mock-composer {
  margin-top: var(--space-4);
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  color: var(--text-3);
  font-size: var(--fs-small);
  box-shadow: var(--shadow-sm);
}

.mock-form {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.mock-step {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.mock-field {
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-input);
  background: var(--bg-card);
  color: var(--text-2);
  font-size: var(--fs-small);
}

.mock-actions {
  display: flex;
  gap: var(--space-8);
  margin-top: var(--space-4);
}

.mock-btn {
  padding: var(--space-4) var(--space-10);
  border: 1px solid var(--btn-secondary-border);
  border-radius: var(--radius-btn-sm);
  background: var(--btn-secondary-bg);
  color: var(--btn-secondary-fg);
  font-size: var(--fs-caption);
}

.mock-btn.is-primary {
  border-color: transparent;
  background: var(--btn-bg);
  color: var(--btn-fg);
  box-shadow: var(--btn-shadow);
}

.mock-drop {
  padding: var(--space-16) var(--space-10);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
  color: var(--text-3);
  font-size: var(--fs-small);
  text-align: center;
}

.mock-stack {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.mock-tabs {
  display: flex;
  gap: var(--space-4);
  padding: 3px;
  border-radius: var(--radius-btn);
  background: var(--bg-sunken);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.mock-tabs span {
  padding: 2px var(--space-8);
  border-radius: var(--radius-tag);
}

.mock-tabs .is-on {
  background: var(--bg-card);
  color: var(--text-1);
  box-shadow: var(--shadow-xs);
}

.mock-line {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
  overflow: hidden;
  text-overflow: ellipsis;
}

.mock-line.is-title {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.mock-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--space-8);
}

.mock-persona {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-4);
  padding: var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
}

.mock-avatar {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.mock-settings {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.mock-nav {
  display: flex;
  gap: var(--space-4);
  padding-bottom: var(--space-6);
  border-bottom: 1px solid var(--border);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.mock-nav .is-on {
  color: var(--brand-text);
  font-weight: var(--fw-semibold);
}

/* ---- 错误 / 降级 / 受限 ---- */

.mock-error {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-6);
  padding: var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-inner);
  background: var(--danger-surface);
}

.mock-error-title {
  color: var(--danger);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.mock-mark {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-6);
}

.mock-readonly {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-size: var(--fs-small);
}

.mock-readonly svg {
  flex: none;
  width: 14px;
  height: 14px;
  fill: none;
  stroke: var(--text-3);
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.mock-locked {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}
</style>
