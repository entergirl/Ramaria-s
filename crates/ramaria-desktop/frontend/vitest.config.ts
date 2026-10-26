/**
 * vitest.config.ts — 单元与组件测试配置
 *
 * 设计特点:
 * - 组件测试需要 DOM，统一 jsdom 环境
 * - 测试文件集中在 tests/，与被测源码目录分离，避免生产构建打入测试代码
 * - 不开全局 API，测试显式从 vitest 导入，便于静态检查未使用符号
 */

import { fileURLToPath } from 'node:url'

import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

/** 仓库根（本目录上溯四级）：parity 校验需读取 `docs/` 下的验收表 */
const repoRoot = fileURLToPath(new URL('../../../../', import.meta.url))

export default defineConfig({
  plugins: [vue()],
  server: { fs: { allow: [repoRoot] } },
  test: {
    environment: 'jsdom',
    include: ['tests/**/*.test.ts'],
    globals: false,
    restoreMocks: true,
  },
})
