<script setup lang="ts">
/**
 * src/views/settings/custom/AppearanceCard.vue — 界面与导航卡
 *
 * 职责:
 * - 用屏幕方位缩略图选择导航圆键的常驻位置（底中 / 四角）
 *
 * 设计特点:
 * - 选项按方位摆在 3 列 2 行的网格里，缩略图上的圆点即圆键落点
 * - 原生单选承载键盘与读屏语义（同组方向键可切换），外观由容器承担
 * - 预设与圆键共用 `stores/nav`：选中即生效，圆键立刻回正
 */

import { computed } from 'vue'

import { NAV_PRESET_LABELS, type NavPreset } from '../../../components/nav/geometry'
import { useNavStore } from '../../../stores/nav'
import SettingsCard from '../SettingsCard.vue'

/** 网格摆放次序：上行 左上 / 右上，下行 左下 / 底中 / 右下 */
const PRESET_ORDER: NavPreset[] = [
  'top-left',
  'top-right',
  'bottom-left',
  'bottom',
  'bottom-right',
]

const nav = useNavStore()

const options = computed(() =>
  PRESET_ORDER.map((value) => ({ value, label: NAV_PRESET_LABELS[value] })),
)

function onPreset(value: NavPreset): void {
  nav.setPreset(value)
}
</script>

<template>
  <SettingsCard
    title="界面与导航"
    description="导航圆键的常驻位置，选中即生效。"
    :editable="false"
  >
    <div class="pos-picker" role="radiogroup" aria-label="导航位置预设">
      <label
        v-for="option in options"
        :key="option.value"
        class="pos-opt"
        :class="{ 'is-active': nav.preset === option.value }"
        :data-pos="option.value"
      >
        <input
          class="pos-input"
          type="radio"
          name="nav-preset"
          :value="option.value"
          :checked="nav.preset === option.value"
          @change="onPreset(option.value)"
        />
        <span class="frame" aria-hidden="true"><i /></span>
        <span class="pos-label">{{ option.label }}</span>
      </label>
    </div>
    <p class="note">拖动圆键可临时移动；双击圆键回到这里选定的位置。</p>
  </SettingsCard>
</template>

<style scoped>
.pos-picker {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--space-8);
}

.pos-opt {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-6);
  padding: var(--space-8) var(--space-6);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  cursor: pointer;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    background-color var(--dur-fast) var(--ease-out),
    color var(--dur-fast) var(--ease-out);
}

.pos-opt:hover {
  border-color: var(--brand-line);
}

.pos-opt:focus-within {
  outline: none;
  box-shadow: var(--focus-ring);
}

.pos-opt.is-active {
  border-color: var(--brand-line);
  background: var(--brand-surface);
  color: var(--brand-text);
  box-shadow: inset 0 0 0 1px var(--brand-line);
}

/* 原生单选视觉隐藏，键盘与读屏语义保留 */
.pos-input {
  position: absolute;
  width: 1px;
  height: 1px;
  opacity: 0;
  clip-path: inset(50%);
}

.frame {
  position: relative;
  width: 100%;
  max-width: 96px;
  height: 34px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-btn-sm);
  background: var(--bg-card);
}

/* 缩略图上的圆点即圆键落点 */
.frame i {
  position: absolute;
  width: 8px;
  height: 8px;
  border-radius: var(--radius-full);
  background: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-surface);
}

.pos-opt[data-pos='top-left'] {
  grid-area: 1 / 1;
}

.pos-opt[data-pos='top-right'] {
  grid-area: 1 / 3;
}

.pos-opt[data-pos='bottom-left'] {
  grid-area: 2 / 1;
}

.pos-opt[data-pos='bottom'] {
  grid-area: 2 / 2;
}

.pos-opt[data-pos='bottom-right'] {
  grid-area: 2 / 3;
}

.pos-opt[data-pos='top-left'] .frame i {
  left: var(--space-6);
  top: var(--space-6);
}

.pos-opt[data-pos='top-right'] .frame i {
  right: var(--space-6);
  top: var(--space-6);
}

.pos-opt[data-pos='bottom-left'] .frame i {
  left: var(--space-6);
  bottom: var(--space-6);
}

.pos-opt[data-pos='bottom'] .frame i {
  left: 50%;
  bottom: var(--space-6);
  transform: translateX(-50%);
}

.pos-opt[data-pos='bottom-right'] .frame i {
  right: var(--space-6);
  bottom: var(--space-6);
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}
</style>
