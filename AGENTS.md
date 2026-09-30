# Ramaria 代码仓库——Agent 工作导航

> 工作根目录：`f:\Ramaria-s\main`（本文件所在目录）
> 目标读者：代码 agent 与开发成员
> 关联文档：`../docs/dev/00-architecture/arch-decisions-unified.md`（架构决策 SSOT）、`../docs/architecture-ai-agent.md`（代码现状 SSOT）、`../docs/dev-2.2/v2.2-plan.md`（当前版本计划）、`../docs/dev/01-setup/setup-code-agent-guide.md`（编码规范与交接格式）
> 状态：骨架（结构收敛期建立；收口时按最终 9 crate 口径定稿）

本文件只做**导航**：告诉你在哪找、改哪里、怎么验证。规范与决策不在此复述，一律引用上游 SSOT。

---

## 1. 工作根目录规则（红线）

- **git**：一切写操作（`commit` / `tag` / `push` / `merge` / `rebase` 等）由项目负责人执行；agent 零 git 写、严禁 push，仅允许只读命令（`status` / `log` / `diff` / `show`）。
- **验证命令**：`cargo check`；`cargo test -j 2 -p <crate>`（`ramaria-cli` 必须带 `-j 2`）；`cargo clippy -j 2 --workspace -- -D warnings`；`cargo fmt --all --check`；前端在 `crates/ramaria-desktop/frontend` 下 `node --test "tests/*.test.js"`。
- **禁止**：`cargo test --workspace` / `--all` / `nextest`（全量由负责人验收）、`cargo run`、`tauri build`、`npm install`、更换工具链。
- **注释规范**：文件头 `//!`（首行 `crates/.../file.rs - Ramaria xxx 模块` + 「设计特点:」列表）；公共项 `///`（小节：职责 / 状态 / 字段约定 / 用法 / 参数 / 返回）；逻辑区块用 `// =====` 分隔；**不写版本号、任务编号、日期**。
- **编码约束**：禁用 let-chains（`collapsible_if = "allow"`）；异步代码不持锁跨 `.await`；不用 `unwrap()` / `expect()` 处理可恢复运行时错误；新模块覆盖率 ≥85%（重改模块 ≥80%）。
- **隐私**：日志与测试产物不含原文全文 / LLM 原始响应 / API key；API key 只进 OS keychain。

> 完整规范见 `../docs/dev/01-setup/setup-code-agent-guide.md`（§2 命令红线、§6 代码质量、§8 安全隐私）。

---

## 2. 必读文档指针

| 场景 | 文档 |
|------|------|
| 任何任务开工前 | `../docs/dev/00-architecture/arch-decisions-unified.md`（跨模块约束、参数基线 §18、候选池 §19） |
| 需要知道"代码在哪、有哪些表/接口" | `../docs/architecture-ai-agent.md`（代码现状 SSOT） |
| 当前版本任务与验收 | `../docs/dev-2.2/v2.2-checklist.md` + `v2.2-plan.md` + `v2.2-decisions.md`（同目录） |
| 契约面（命令 / 子命令 / 工具 / 前端调用） | `../docs/dev-2.2/test/contract-baseline.md`（入口契约冻结基线） |
| 算法与数值口径 | `../docs/reports/对话-事件-人格画像系统技术报告.md` |
| 对话 / 记忆 / 人格 / 导入 模块细节 | `../docs/dev/02-chat-system/`、`03-memory-pipeline/`、`04-personality/`、`05-import/`、`07-mcp/` |
| 版本方向 | `../docs/dev-3.0/roadmap.md` |

---

## 3. crate 地图（9 个）

| crate | 职责（一句话） | 入口文件 | 导航 |
|-------|----------------|----------|------|
| `ramaria-core` | 类型 / trait / 错误 / 配置（零 I/O） | `src/lib.rs` | [README](crates/ramaria-core/README.md) |
| `ramaria-storage` | SQLite schema / Repository / migration | `src/lib.rs`、`migrations/` | [README](crates/ramaria-storage/README.md) |
| `ramaria-memory` | L0→L3 管线、检索融合、Prompt 装配、共用召回 | `src/lib.rs` | [README](crates/ramaria-memory/README.md) |
| `ramaria-llm` | LLM provider、SSE 流式、keychain、原生嵌入 | `src/lib.rs` | [README](crates/ramaria-llm/README.md) |
| `ramaria-service` | 唯一能力层：引擎装配 + 用例（召回 / 生成 / 写入 / 封存 / 空闲 / 人格） | `src/lib.rs` | [README](crates/ramaria-service/README.md) |
| `ramaria-mcp` | MCP 协议壳（stdio，6 工具） | `src/lib.rs` | [README](crates/ramaria-mcp/README.md) |
| `ramaria-cli` | CLI 入口（19 子命令）+ 评估探针 | `src/main.rs` | [README](crates/ramaria-cli/README.md) |
| `ramaria-desktop` | Tauri 2 桌面壳（60 command 注册 / 托盘 / 前端资源） | `src/lib.rs` | [README](crates/ramaria-desktop/README.md) |
| `ramaria-importer` | 聊天记录导入（QQ Chat Exporter v6.x JSON） | `src/lib.rs` | [README](crates/ramaria-importer/README.md) |

依赖方向（不可反向引用；详见决策基线 §3.2）：

```text
ramaria-cli / ramaria-desktop            ramaria-mcp
         └───────────────┬─────────────────┘
                         ↓
                  ramaria-service
                         ↓
ramaria-memory / ramaria-llm / ramaria-importer
         ↓
   ramaria-storage
         ↓
    ramaria-core
```

入口（CLI / 桌面 / MCP）全部直连 `ramaria-service`；服务层为唯一能力层，禁止依赖任何入口层。

---

## 4. 改动导航（高频场景 → 落点）

| 要改什么 | 落点（收敛后以服务层为准） |
|----------|---------------------------|
| 对话生成（含流式事件） | `ramaria-service/src/chat.rs`；桌面事件桥 `ramaria-desktop/src/events.rs` |
| 封存 / L1 摘要 / 封存钩子 | `ramaria-service/src/seal.rs`、`src/hooks.rs`；摘要算法 `ramaria-memory/src/l1/` |
| 召回 / 检索融合 / 衰减 | 共用实现 `ramaria-memory/src/recall.rs`（+ `retriever/`、`bm25.rs`、`rrf.rs`）；分层装配 `ramaria-service/src/recall.rs` |
| 检索索引懒加载 / 代次刷新 | `ramaria-service/src/index.rs` |
| 外部对话回流写入 | `ramaria-service/src/ingest.rs` |
| 空闲检查 / L2-L3 调度 | `ramaria-service/src/idle.rs`、`src/l2.rs`（收敛后统一为 Lifecycle） |
| 会话生命周期（活跃指针 / 关闭） | `ramaria-service/src/session.rs`（收敛后 `src/lifecycle/`） |
| 数据库 schema / 查询 / migration | `ramaria-storage/migrations/` + `ramaria-storage/src/repo/` |
| 配置默认值与参数口径 | `config/default.toml`（数值 SSOT：决策基线 §18） |
| 人格 / 行为 / 知识 / 风格 | `ramaria-memory/src/{behavior,fact,style,inference}/` + `ramaria-service/src/persona.rs` |
| 桌面命令与页面 | `ramaria-desktop/src/commands/*.rs` + `frontend/js/`（调用封装 `api.js`） |
| CLI 子命令与输出 | `ramaria-cli/src/main.rs`（clap 定义）+ `src/commands/`；`--json` 信封 `src/json.rs` |
| MCP 工具（schema / 错误映射） | `ramaria-mcp/src/tools/` + `src/server.rs`（只做协议包装，不含业务逻辑） |
| 导入（QQ 记录） | 解析与写入实现 `ramaria-importer/src/qq/`；服务层用例 `ramaria-service/src/import.rs`（`importer` feature） |
| 模型下载 / 校验 / 嵌入 | `ramaria-llm/src/embedding/`；服务层编排 `ramaria-service/src/model.rs`（收敛后） |
| 版本检查 / 诊断导出 | `ramaria-service/src/{update,diagnostics}.rs`（收敛后） |
| 契约面变更前 | 先读 `../docs/dev-2.2/test/contract-baseline.md`（零变更口径） |

> 本表为初版高频映射（骨架）。收口阶段按最终文件归属定稿，并补充"改动 → 必跑测试"列。

---

## 5. 验证与提交前自查

1. `cargo check`（编译）
2. `cargo test -j 2 -p <改动 crate>`（分 crate；CLI 必带 `-j 2`；改到 `ramaria-service` 的导入用例时加 `--features importer`）
3. `cargo clippy -j 2 --workspace -- -D warnings`（静态检查）
4. `cargo fmt --all --check`（格式）
5. 涉及前端：`cd crates/ramaria-desktop/frontend; node --test "tests/*.test.js"`
6. 涉及契约面：与 `../docs/dev-2.2/test/contract-baseline.md` 逐项对照
7. 结构收敛期附加：纯结构重构不夹带行为变更；发现问题登记候选池，不顺手修

> 本机已知环境问题：`cargo test` 在 `-j 2` 下编译大 crate（`ramaria-cli` / `ramaria-desktop`）时偶发 `os error 1455（页面文件太小）` 与链接器 `STATUS_STACK_BUFFER_OVERRUN`，表现为 `E0463 / E0462 / E0786` 等"找不到 crate / 元数据无效"的级联假错误；处置为重试或改用 `-j 1`，与代码无关。

全量 `cargo test --workspace` 与 git 提交由项目负责人执行。
