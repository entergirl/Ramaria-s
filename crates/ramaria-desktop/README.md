# ramaria-desktop

> 定位：Tauri 2 桌面壳——窗口 / 托盘 / 通知、57 项 command 注册（56 业务 + 1 托盘动作）、前端资源（原生 JS）。
> 上游 SSOT：`../../../docs/dev-2.3/test/contract-baseline-2.3.md`（§2 command 签名 / §3 前端调用面）、`../../../docs/dev/06-desktop/desktop-design.html`（UI 设计参考）。

## 职责

- **Tauri 壳与装配**：`lib.rs` 的 `.manage(state)` 与 `generate_handler![...]`（57 项注册）；`main.rs` 进程入口。
- **命令层**：`commands/*.rs` 做参数转换、调用服务层用例、结果映射（`Result<T, String>`）；命令函数必须留在注册路径所在文件（`generate_handler!` 按原路径引用），视图映射可外移。
- **宿主专属能力**：`tray.rs`（托盘与关闭动作）、`notification.rs`（系统通知）、`path_guard.rs`（路径脱敏/校验）、`events.rs`（流式事件 → Tauri event 桥）。
- **前端**：`frontend/`（原生 JS + CSS，零依赖）；`js/api.js` 为 command 调用封装层（55 条映射）。
- **开发数据目录**：`.ramaria-dev/`（`cargo tauri dev` 时的数据根）。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/main.rs` | 进程入口（薄） | — |
| `src/lib.rs` | `run()` 装配（插件 / state / handler / setup）、`generate_handler!` 注册表 | — |
| `src/events.rs` | 流式事件 → Tauri event 桥（`chat-done` 全文） | 内联 |
| `src/webview.rs` | WebView2 附加启动参数清理（release 剥离远程调试端口） | 内联 |
| `src/tray.rs` / `src/notification.rs` | 托盘与关闭动作 / 系统通知 | 内联 |
| `src/path_guard.rs` + `src/path_guard/` | 路径脱敏 / 校验；`privacy_audit_tests` 为常驻隐私审计 | `src/path_guard/tests.rs`、`privacy_audit_tests.rs` |
| `src/commands/` | 命令实现（17 模块）：chat / session / memory（+ `memory_view.rs` 视图映射）/ config / mcp / export / index_cmd / import_cmd / persona / rules / keywords / style / evaluation / diagnostics / setup / dialog | 内联测试段 |
| `frontend/js/` | 前端逻辑：根模块 6 + `views/`（8 视图：chat / memory / persona / rules / import / settings / setup / debug）+ `components/` 7 + `utils/` 5；`api.js` 为调用封装 | `frontend/tests/` |
| `frontend/css/` | 设计令牌与样式（16 个文件） | — |
| `frontend/tests/` | 10 个测试文件 + helpers：`settings-defaults`（与 default.toml 逐键）/ `store-keys`（Store 字段注册扫描）/ `markup-conventions` / `markdown` / `bubble` / `dom` / `format` / `probe` / `mcp-snippets` / `session-source-tag` | `frontend/tests/` |
| `.ramaria-dev/` | 开发模式数据目录（不随发行） | — |

## 相邻契约

- 依赖：`ramaria-service`（含 `importer` feature）+ `ramaria-importer` + `ramaria-core`；Tauri 框架。
- **禁止**：记忆业务逻辑（一律经服务层用例）；`ramaria-service` 反向依赖本 crate。
- 契约面：command 名 / 参数 / 返回与契约基线逐项一致（57 项注册 / `api.js` 55 条封装 + 旁路调用，见基线 §2 / §3）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 / 修改 command | `src/commands/<group>.rs` + `src/lib.rs` 注册表 | 契约基线 §2 + `frontend/js/api.js` + 前端调用 |
| 流式事件桥 | `src/events.rs` | CLI 侧事件流同源（服务层 `StreamEvent`） |
| 托盘 / 通知 / 路径保护 | `src/tray.rs` / `notification.rs` / `path_guard.rs` | — |
| 前端页面与样式 | `frontend/js/`、`frontend/css/` | `node --test "tests/*.test.js"` |

## 验证

```bash
cargo test -j 2 -p ramaria-desktop
cd crates/ramaria-desktop/frontend; node --test "tests/*.test.js"
```
