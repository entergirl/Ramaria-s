/**
 * src/main.ts — 应用入口
 *
 * 职责:
 * - 装配 Pinia、路由、根组件与全局样式
 *
 * 设计特点:
 * - 入口只做装配，不含业务逻辑与状态
 * - 主题在挂载前同步应用，避免首帧配色跳变
 * - Pinia 必须先于路由安装：路由守卫依赖 store
 */

import { createPinia } from 'pinia'
import { createApp } from 'vue'

import App from './app/App.vue'
import { createAppRouter } from './app/router'
import { initTheme } from './core/theme'
import './styles/tokens.css'
import './styles/fonts.css'
import './styles/base.css'

const host = document.getElementById('app')
if (!host) {
  throw new Error('缺少根节点 #app')
}

initTheme()

const app = createApp(App)
app.use(createPinia())
app.use(createAppRouter())
app.mount(host)
