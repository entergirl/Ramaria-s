---
name: rust
description: 
tools: list_dir, search_file, search_content, read_file, read_lints, replace_in_file, write_to_file, execute_command, delete_file, connect_cloud_service, preview_url, web_fetch, use_skill, web_search, automation_update, task
agentMode: manual
enabled: true
enabledAutoRun: true
---
# Ramaria Rust Agent 工作指导

> 适用对象：代码生成 agent  
> 工作根目录：`F:\Ramaria\Ramaria v0.x\rust`  
> 最高优先级目标：少返工、可编译、可测试、可维护  
> 当前版本：v1.1（活跃任务清单 → `docs/dev-1.1/v1.1-任务清单.md`）
---

## 1. 基本原则

Agent 在生成代码前必须先思考，再写代码。不要看到任务就直接大段输出实现。

每次开发都按以下顺序执行：

1. 阅读任务清单，确认任务编号和阶段目标。
2. 阅读相关计划和决策文档。
3. 阅读现有 Rust 代码和必要的 Python 参考代码。
4. 明确边界、输入、输出、错误处理和测试方式。
5. 先给出简短实现方案。
6. 再修改代码。
7. 自查代码是否清晰、规范、可编译、符合 crate 边界。
8. 列出需要用户亲自运行的终端命令。

不要为了“快速完成”跳过设计判断。Rust 代码必须完整、清晰、严谨，尽量一次实现到可编译、可通过 CI 的状态。

## 2. 终端命令规则

Agent **禁止**运行构建/测试/格式化/安装依赖/启动服务类命令，包括：

Agent 不主动运行以下命令：

```bash
cargo build
cargo test
cargo clippy
cargo fmt
cargo nextest
cargo llvm-cov
cargo run
npm install
npm run
tauri build
```

Agent 可以在回复中列出建议用户运行的命令、运行目录和预期结果。例如：

```bash
cd F:\Ramaria\Ramaria v0.x\rust
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

如果代码修改后需要验证，Agent 必须明确说明：

- 需要运行哪些命令。
- 为什么需要这些命令。
- 预期成功结果是什么。
- 如果失败，优先检查哪些文件。

允许 Agent 使用只读命令查看文件和目录，例如读取文件、列目录、搜索文本。但不要运行构建、测试、格式化、安装依赖或启动服务类命令。

## 3. 工作目录和文件边界

允许修改：

- `rust/` 下的 Rust 代码、配置和文档。
- 与 Rust 重构直接相关的文档。

---

## 4. 必读文档

**任务执行前**至少阅读以下文件（路径相对于工作根目录 `F:\Ramaria\Ramaria v0.x\rust`）：

| 优先级 | 文档 | 用途 |
|--------|------|------|
| 🔴 | `docs/dev-1.1/v1.1-任务清单.md` | 当前活跃任务（94 项，Phase 1-9） |
| 🔴 | `docs/dev-1.1/v1.1-决策列表.md` | v1.1 设计 SSOT |
| 🟡 | `docs/dev/rust-rewrite-analysis.md` | 整体架构分析 |
| 🟡 | `docs/dev/rust重构决策列表.md` | v1.0 设计决策 |
| 🟢 | `docs/dev-1.0/development-task-checklist.md` | v1.0 任务（历史参考） |
| 🟢 | `docs/dev-1.0/rust-migration-map.md` | Python→Rust 模块映射 |
| 🔵 | `docs/code-review-report-v1.0.1.md` | v1.0.1 审查发现（32 项） |


---

## 5. 架构边界

**依赖方向**（不可反向引用）：

```
ramaria-cli / ramaria-desktop
         ↓
    ramaria-app
         ↓
ramaria-memory / ramaria-llm / ramaria-importer (v1.1新增)
         ↓
   ramaria-storage
         ↓
    ramaria-core
```

**crate 职责速查**：

| Crate | 职责 | 禁止依赖 |
|-------|------|----------|
| `ramaria-core` | 类型、trait、错误、配置、状态机 | sqlx, reqwest, tokio, 数据库, 网络 |
| `ramaria-storage` | SQLite migration、CRUD、索引存取 | LLM, 业务逻辑 |
| `ramaria-memory` | L1/L2/L3、衰减、RRF、图谱、检索 | 具体的 LLM provider |
| `ramaria-llm` | LM Studio/DeepSeek/OpenAI client | UI |
| `ramaria-app` | 用例编排，CLI/Desktop 共用 | 具体的 UI 框架 |
| `ramaria-cli` | 命令行入口，参数解析+展示 | Tauri |
| `ramaria-desktop` | Tauri thin shell，Command/Event/托盘 | 记忆业务逻辑 |
| `ramaria-importer` | 聊天记录导入 (feature gate) | UI |

不要跨层偷懒。例如：

- 不要在 `ramaria-desktop` 里写记忆业务逻辑。
- 不要在 `ramaria-core` 里引入 sqlx、reqwest、tokio。
- 不要在 `ramaria-memory` 里硬编码 DeepSeek/OpenAI。
- 不要让 UI 层直接操作 sqlx 连接池。

## 6. Rust 代码质量要求

生成的 Rust 代码必须满足：

- 类型清晰，错误类型明确。
- 公共 API 命名稳定、语义直观。
- 模块职责单一。
- 不引入无必要抽象。
- 不用 `unwrap()` / `expect()` 处理可恢复运行时错误。
- 测试代码中可以使用 `expect()`，但错误信息要具体。
- 不吞掉错误；使用结构化错误返回。
- 不记录 API key、完整 prompt、完整用户消息。
- 异步代码不持有锁跨 `.await`。
- 避免全局可变状态。
- 优先小函数、小模块，避免巨型文件。

注释原则：

- 注释风格参考旧 Python 文件，例如 `src/ramaria/logger.py`。
- Rust 文件开头必须使用 crate/module 文档注释 `//!`，格式保持清晰直接：
  - 第一行：`//! rust/crates/.../file.rs - Ramaria xxx 模块`
  - 空行。
  - `//! 设计特点:`
  - 使用短横线列出 4-6 条核心职责、边界和设计约束。
- 文件头示例：
  ```rust
  //! rust/crates/ramaria-core/src/error.rs - Ramaria 统一错误管理模块
  //!
  //! 设计特点:
  //! - 标准化错误分类: Config / Storage / Llm / Privacy / Index / Validation / Io / Unsupported
  //! - 统一公共 API 返回类型: `RamariaResult<T>`
  //! - 支持 trace_id 贯穿请求、检索、LLM 调用和后台任务生命周期
  //! - 支持 source 错误链，保留底层错误上下文，便于日志和 UI 诊断
  //! - 提供便捷构造器和常用 From 实现，减少上层 crate 的重复样板代码
  ```
- 逻辑区块使用类似 Python 文件的视觉分隔，不在分隔处写长说明：
  ```rust
  // =========================================================
  // 标准化错误构造器
  // =========================================================
  ```
- 详细说明写在具体代码对象上，而不是堆在模块分隔符下面。
- 公共 struct、enum、trait、函数和重要字段按 Rust 惯例使用 `///` 注释。
- struct/enum/trait 注释优先使用这些小节：
  - `职责:`
  - `状态:`
  - `格式:`
  - `字段约定:`
  - `实现要求:`
  - `安全约束:`
- 函数和方法注释优先使用这些小节：
  - `用法:`
  - `参数:`
  - `返回:`
  - `说明:`
- 函数注释示例：
  ```rust
  /// 创建一条新消息。
  ///
  /// 参数:
  /// - `session_id`: 消息所属 Session。
  /// - `role`: 消息角色。
  /// - `content`: 原始文本内容。
  /// - `source`: 本地或线上来源。
  ///
  /// 返回:
  /// - 带新 UUID、当前创建时间且无 fingerprint 的消息。
  ```
- 类型注释示例：
  ```rust
  /// 对话会话。
  ///
  /// 职责:
  /// - 表示一次连续对话生命周期。
  /// - 承载 L0 消息归属关系。
  /// - 为 session 结束后的 L1 摘要生成提供边界。
  ///
  /// 状态:
  /// - `ended_at = None`: 会话仍在进行中。
  /// - `ended_at = Some(...)`: 会话已关闭，可触发 L1 摘要。
  ```
- 不在长期代码注释中保留 `T-CORE-001`、`T-STORAGE-001` 这类任务编号；任务编号只写在任务清单、PR、提交说明或完成记录中。
- 注释应说明“为什么存在、承担什么边界、输入输出有什么约定”，不要重复代码字面含义。
- 复杂逻辑前加简短说明。
- 不写“把值赋给变量”这类空注释。
- POC 代码进入正式实现时必须清理或标注。

## 7. 测试要求

每个功能任务都要考虑测试。

优先测试：

- core 类型 serde。
- config 默认值和覆盖。
- error display/source。
- storage migration 和 CRUD。
- memory 纯函数：衰减、RRF、排序、截断。
- LLM client 的请求构造和 SSE parser。
- app 编排的 mock backend 路径。

CI 不跑：

- 真实 LLM。
- embedding 大模型下载。
- Tauri 打包。
- 需要用户凭证的 keychain 真实写入测试。

如果某任务暂时不能自动化测试，必须在完成说明中写明：

- 为什么不能自动化。
- 建议用户如何手动测试。
- 后续如何补测试。

## 8. 安全与隐私

必须遵守：

- API key 只进 OS keychain。
- 非敏感配置才能写入 config。
- 日志默认不记录完整用户消息、记忆、prompt。
- 线上 provider 按 provider + base_url 做隐私确认。
- provider/base_url 变化时重新确认。
- 不在测试 fixture 中写真实 API key。
- 不把 `.env` 或本地密钥内容写入文档。

前端渲染：

- 支持 Markdown 可以，但禁用原始 HTML。
- LLM 输出必须经过安全渲染或 sanitize。
- 不加载远程脚本。

## 9. 任务推进方式

处理任务时使用这个模板：

```markdown
任务：T-XXX-000

理解：
- 本任务要解决什么问题：
- 涉及 crate：
- 参考文件：

方案：
- 数据结构：
- 主要函数/API：
- 错误处理：
- 测试策略：

修改：
- 文件 1：
- 文件 2：

需要用户运行：
- 命令：
- 预期：
```

完成后更新任务清单中的状态或追加完成记录。

## 10. 遇到不清楚的问题

以下情况必须先问项目负责人，不要自行决定：

- 会影响 schema 的字段、索引、表关系。
- 会改变 crate 依赖方向。
- 会引入新外部依赖。
- 会改变 v1.0 范围。
- 会修改 Python 旧代码。
- 会记录或传输用户隐私数据。
- 会改变 API key 存储方式。
- 会引入运行时插件机制。
- 会改变首次配置完成条件。
- 会把 deferred 功能提前实现。

提问时要给出：

- 当前不确定点。
- 2-3 个可选方案。
- 推荐方案和理由。
- 不决策的影响。

## 11. 用户手动测试交接格式

每次代码修改完成后，Agent 用以下格式交接：

````markdown
已修改：
- 文件：
- 内容：

未运行终端测试：
- 按项目规则，构建/测试命令由你亲自运行。

建议你运行：
```bash
cd F:\Ramaria\Ramaria v0.x\rust
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

预期：
- 所有测试通过。
- clippy 无 warning。
- fmt check 无 diff。
````

如果用户反馈测试失败，Agent 应先阅读错误输出，再做最小修复。不要无依据重构。
