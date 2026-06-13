# 变更日志

本文档记录 Ramaria Rust 版的所有显著变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

---

## [1.0.0] - 2026-06-12

### 核心特性

#### 分层记忆管线（L0→L1→L2→L3）

- L0 原始消息层：永久保留所有对话消息，标记发言人，按时间排序
- L1 单次摘要层：session 结束后 LLM 自动压缩，生成结构化摘要（summary + keywords + time_period + atmosphere + valence + salience）。关键词从 keyword_pool 优先选择，确保长期收敛
- L2 事件提取层：未吸收 L1 ≥ 5 条或超 7 天触发，提取离散事件（含 8 个推断属性）。LLM 不可用时自动回退到规则式降级生成
- L3 人格画像层：surface/behavioral/core 三层分级，share 分级（private/trusted/public）控制 RAG 注入范围。Phase A 统计推断 + Phase B LLM 推断 + Phase C 增量漂移检测
- 冷启动流程：首次加载人格时自动注入知识背景
- 全量重建管线：支持切换 LLM 后端后从 L0 重新提取全部 L1/L2/L3

#### 三通道混合 RAG 检索

- BM25 全文检索：自研 Rust 实现，关键词精确匹配
- 向量检索：暴力搜索余弦相似度 + Qdrant（可选），语义相似度检索
- 知识图谱检索：BFS 遍历实体关系图，召回关联历史记忆
- RRF 倒数排名融合：三通道结果加权合并
- Ebbinghaus 遗忘曲线衰减：记忆检索权重随时间衰减，salience 调制衰减速度
- Persona-Aware 过滤：按人格画像 share 分级过滤可注入的记忆

#### 事件→性格推断管线

- Phase A 统计推断：高置信度事件特征均值计算
- Phase B LLM 推断：示例精选→聚类→推断→校准四步流水线
- Phase C 增量更新：计算 drift 漂移度，确认迁移路径
- 置信度追踪：每项特征关联 evidence，可溯源至原始事件

#### LLM Provider 层

- LM Studio 适配器：无 API Key，完全本地推理
- DeepSeek 适配器：支持 deepseek-v4
- OpenAI 适配器：兼容所有 OpenAI API 格式服务
- SSE 流式传输：futures channel + tokio spawn 异步架构
- OS 凭据管理器：Windows Credential Manager 安全存储 API Key
- 统一重试策略：指数退避，鉴权错误不重试

#### Tauri 2 桌面应用

- 原生 Windows 窗口：960×720 默认尺寸，最小 640×480
- 粉蓝双色设计系统：CSS Tokens 变量体系，暗/亮双主题
- 系统托盘：最小化到托盘，托盘菜单快捷操作
- 通知推送：新消息通知、后台处理完成通知
- 关闭确认弹窗：托盘最小化 / 完全退出二选一
- 配置向导：5 步引导（后端选择→API 配置→测试连接→人格选择→完成）
- 记忆查看器：L1/L2/L3 分页浏览，支持删除 + 二次确认
- UI 组件库：Toast / Modal / Skeleton / Markdown 渲染器
- Markdown 白名单 sanitizer + CSP + XSS 防护

#### CLI 工具

- 9 个子命令：`setup` / `ask` / `chat` / `memory` / `session` / `config` / `persona` / `index` / `export`
- 交互式 REPL：色彩输出，历史记录
- 流式输出：`--no-stream` 关闭流式，`--json` 输出原始 JSON
- 数据导出：支持 JSON / Markdown 格式
- 隐私确认：`--yes` 跳过确认

---

### 新增功能

#### 存储层

- 23 张表 SQLite schema，一次性 migration
- 19 个 Repository，手动行映射避免 sqlx derive 侵入 core
- WAL 模式，多连接读写并发
- 数据目录：默认 `%APPDATA%\Ramaria\`，环境变量 `RAMARIA_DATA_DIR` 覆盖

#### 配置管理

- 统一 RamariaConfig 配置结构
- SQLite settings 表持久化，支持 CLI 读取和修改
- 多后端配置（LM Studio / DeepSeek / OpenAI）
- 人格 TOML 文件（`config/personas/*.toml`）
- 隐私确认按 `provider + base_url` 粒度管理

#### 安全

- API Key 存储在 Windows Credential Manager
- 日志不记录完整对话内容，敏感字段截断或哈希
- CSP 内容安全策略，前端零 eval()
- Markdown 白名单标签 + 移除事件处理器 + 禁止危险协议
- CLI 路径穿越防护
- 本地模式完全离线，不发起外部网络请求

#### 错误处理

- 8 种错误变体（Config/Storage/Llm/Privacy/Index/Validation/Io/Unsupported）
- 错误到用户友好提示的映射（ErrorHint）
- CLI 错误上下文，带原文引用的错误信息

---

### 工程改善

#### 项目架构

- 7 个 crate 分层设计（core → storage/llm → memory → app → cli/desktop）
- Workspace resolver="3"，edition="2024"，MSRV 1.85
- 零 I/O 依赖的 core 层作为类型边界
- async-trait 抽象，支持 mock 测试

#### 测试

- 600+ 个测试函数，覆盖全部 7 个 crate
- 集成测试目录 `tests/`，含 fixture 数据和 mock backend
- CI：build + test + clippy(`-D warnings`) + fmt(`--check`)
- Smoke test 清单：11 类 83 项

#### 文档

- README：项目总览、架构图、模块职责表、分层记忆详解、核心创新设计
- 桌面使用指南：安装→配置→对话→记忆→设置→故障排除
- CLI 使用指南：9 个子命令完整参考
- 隐私说明：数据流向、安全措施、权限说明
- 发行说明模板：标准化 13 节结构
- 4 个 GitHub Issue 模板（Bug / Feature / Help / Config）

---

### 已知限制

- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件，正式图标待设计师提供
- 不支持 LLM 对话"重新生成"功能
- MCP Bridge / 导入器 / 自动更新 / 多角色 GUI 等功能已延后

---

## 版本历史概览

| 版本 | 日期 | 说明 |
|------|------|------|
| [v1.0.0](#100---2026-06-12) | 2026-06-12 | Rust 重写完成，首个正式发布版本 |
| v0.7.0 | 2026-05-09 | Python 版最终功能版本（维护模式） |

> Python v0.3.x–v0.7.x 的完整变更记录见项目根目录 [`CHANGELOG.md`](../../CHANGELOG.md)。
