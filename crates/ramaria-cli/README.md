# ramaria-cli

> 定位：命令行入口（clap derive，19 个子命令）+ 评估探针（例外登记：允许直连 `ramaria-memory` 算法原语）。
> 上游 SSOT：`../../../docs/dev-2.5/test/contract-baseline-2.5.md`（差异式基线；CLI 面：子命令 / `--json` 信封 / 退出码，逐项底本 = 2.3 基线 §5）、`../../../docs/dev-2.2/v2.2-decisions.md`（D-V22-009 探针例外）。

## 职责

- **参数解析与分发**：`cli.rs` 定义 `Cli` 与 `Commands`；`dispatch.rs` 分发到 `commands/`；`main.rs` 仅进程入口。
- **`--json` 信封**：`json.rs` 统一 `{ ok: true, data }` / `{ ok: false, error: { code, message } }`；退出码 0 / 2 / 3 / 4。
- **子命令实现**：`commands/*` 每个子命令一个模块；只做"调服务层用例 + 输出格式化"。
- **评估探针**：`commands/probe/`（数据集构建 / 批量运行 / 评估 / 报告），属研发工具链；例外登记（D-V22-009）允许直连 `ramaria-memory` 算法原语与数据读取，不直连 LLM provider（LLM 经服务层装配）。
- **隐私确认**：`privacy.rs` 提供 CLI 侧线上 provider 确认流程。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/main.rs` | 进程入口（薄） | — |
| `src/lib.rs` | crate 根：模块声明与 re-export | — |
| `src/cli.rs` | clap 定义：`Cli` 全局选项、`Commands` 枚举、`help_groups()` | `src/tests.rs` |
| `src/dispatch.rs` | 子命令分发与全局选项装配 | `src/tests.rs` |
| `src/json.rs` | `emit_ok` / `emit_err`（信封与退出码口径） | `src/tests.rs` |
| `src/ui.rs` / `src/util.rs` | 终端输出（表格 / 颜色 / 进度）/ 通用工具 | `src/tests.rs` |
| `src/privacy.rs` | CLI 侧隐私确认流程 | 内联 |
| `src/commands/` | 19 个子命令实现（ask / chat / setup / memory / utt / index_cmd / import_cmd / export / session / config / persona / rule / style / fact / keyword_cmd / diagnostics / status / probe / mcp） | `memory/tests.rs`、`setup/tests.rs`、`rule/tests.rs`；其余内联 |
| `src/commands/probe/` | 评估探针：`types` / `dataset/`（构建）/ `run/`（批量运行）/ `evaluate/`（评分）/ `report/`（统计 / 消融 / TOST / 校准 / 知识质量 / 渲染）/ `tests/` | `src/commands/probe/tests/` |
| `src/commands/rule/` | 规则子命令：`manage`（增删改查）/ `incremental` / `clusters`（工具例外：只读事件 + 聚类原语） | `src/commands/rule/tests.rs` |
| `tests/` | 集成测试：`command_tests`（薄入口 + `command_tests/` 四域）+ export / keyword / memory_l2 / probe / rule / session / style / ui 各域 | `tests/`；夹具 `tests/common/` |
| `examples/` | 研发用 PoC（`keychain_poc` / `vector_poc`），不参与构建产物 | — |

## 公共入口

| 模块 | 内容 |
|------|------|
| `cli.rs` | `Cli` 全局选项（`--db` / `--yes` / `--skip-validate` / `--json` / `--quiet`）、`Commands`、`help_groups()` |
| `commands/` | 19 个子命令实现 |
| `json` | `emit_ok` / `emit_err`（信封与退出码口径） |
| `ui` / `util` / `privacy` | 输出格式化、通用工具、隐私确认 |

## 相邻契约

- 依赖：`ramaria-service`（用例，含 `importer` feature）+ `ramaria-core`；`ramaria-mcp`（`mcp serve` 宿主装配）；`ramaria-memory`（探针 / 工具例外算法原语）；`ramaria-importer`（导入解析）；`ramaria-storage` 仅测试夹具使用。
- **禁止**：Tauri 依赖；写业务编排逻辑（除探针 / 工具例外）。
- 与桌面共享同一数据库时注意多进程写锁（busy_timeout 等待语义）。

## 直查口径（摘要）

入口不得经存储句柄（`engine.storage()` 等）做业务查询 / 业务写入；例外仅两类，新增须登记：

- **探针**：`commands/probe/**` 直连 `ramaria-memory` 算法原语与评估数据读取；
- **工具**：`commands/rule/clusters.rs` 的 `clusters` 直连事件只读与聚类原语（调参 / 诊断，不属产品能力面）。

清单与判定标准（SSOT）：`../../../docs/dev/01-setup/setup-code-agent-guide.md` §4；新增例外须先在版本决策记录登记，再在清单追加。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 / 修改子命令 | `src/cli.rs`（clap 定义）+ `src/dispatch.rs` + `src/commands/<cmd>.rs` | 契约基线 §4 + `help_groups()` 覆盖测试 |
| `--json` 字段 | `src/json.rs` 与各命令载荷 | 契约基线 §4.3 |
| 探针数据集 / 变体 | `src/commands/probe/` | 评估报告；不写入仓库工作区 |
| 终端输出样式 | `src/ui.rs` | — |

## 验证

```bash
cargo test -j 2 -p ramaria-cli   # 必须带 -j 2（否则 mmap 耗尽并出现级联假错误）
```
