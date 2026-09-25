# ramaria-cli

> 定位：命令行入口（clap derive，19 个子命令）+ 评估探针（例外登记，允许直连算法原语）。
> 上游 SSOT：`../../../docs/dev-2.2/test/contract-baseline.md`（§4 CLI 基线：子命令 / `--json` 信封 / 退出码）、`../../../docs/dev-2.2/v2.2-decisions.md`（D-V22-009 探针例外）。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿；收敛后入口全部改经 `ramaria-service` 用例）。

## 职责

- **参数解析与展示**：`main.rs` 的 `Commands` 枚举定义全部子命令；`ui.rs` 负责终端输出（表格 / 颜色 / 进度）。
- **`--json` 信封**：`json.rs` 统一 `{ ok: true, data }` / `{ ok: false, error: { code, message } }`；退出码 0 / 2 / 3 / 4。
- **子命令实现**：`commands/*` 每个子命令一个模块；收敛后只做"调服务层用例 + 输出格式化"。
- **评估探针**：`commands/probe/`（数据集构建 / 批量运行 / 评估 / 报告），属研发工具链，允许直连 `ramaria-memory` / `ramaria-llm` 算法原语（例外登记）。
- **隐私确认**：`privacy.rs` 提供 CLI 侧线上 provider 确认流程。

## 公共入口

| 模块 | 内容 |
|------|------|
| `main.rs` | `Cli` 全局选项（`--db` / `--yes` / `--skip-validate` / `--json` / `--quiet`）、`Commands`、`help_groups()` |
| `commands/` | 19 个子命令实现（ask / chat / setup / memory / blocks / index / import / export / session / config / persona / rule / style / fact / keyword / diagnostics / status / probe / mcp） |
| `json` | `emit_ok` / `emit_err`（信封与退出码口径） |
| `ui` / `util` / `privacy` | 输出格式化、通用工具、隐私确认 |

## 相邻契约

- 依赖：收敛后为 `ramaria-service`（用例）+ `ramaria-core`；探针模块额外直连 `ramaria-memory` / `ramaria-llm`（例外）。
- **禁止**：Tauri 依赖；写业务编排逻辑（除探针例外）。
- 与桌面共享同一数据库时注意多进程写锁（busy_timeout 等待语义）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 / 修改子命令 | `src/main.rs`（clap 定义）+ `src/commands/<cmd>.rs` | 契约基线 §4 + `help_groups()` 覆盖测试 |
| `--json` 字段 | `src/json.rs` 与各命令载荷 | 契约基线 §4.3 |
| 探针数据集 / 变体 | `src/commands/probe/` | 评估报告；不写入仓库工作区 |
| 终端输出样式 | `src/ui.rs` | — |

## 验证

```bash
cargo test -j 2 -p ramaria-cli   # 必须带 -j 2（否则 mmap 耗尽并出现级联假错误）
```
