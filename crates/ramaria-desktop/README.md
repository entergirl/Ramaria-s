# ramaria-desktop

> 定位：Tauri 2 桌面壳——窗口 / 托盘 / 通知、61 项 command 注册（60 业务 + 1 托盘动作）、前端资源（原生 JS）。
> 上游 SSOT：`../../../docs/dev-2.2/test/contract-baseline.md`（§2 command 签名 / §3 前端调用面）、`../../../docs/dev/06-desktop/desktop-design.html`（UI 设计参考）。

## 职责

- **Tauri 壳与装配**：`lib.rs` 的 `.manage(state)` 与 `generate_handler![...]`（61 项注册：60 业务 command + 1 托盘动作）；`main.rs` 进程入口。
- **命令层**：`commands/*.rs` 做参数转换、调用例、结果映射（`Result<T, String>`）。
- **宿主专属能力**：`tray.rs`（托盘与关闭动作）、`notification.rs`（系统通知）、`path_guard.rs`（路径脱敏/校验）、`events.rs`（流式事件 → Tauri event 桥）。
- **前端**：`frontend/`（原生 JS + CSS，零依赖）；`js/api.js` 为 command 调用封装层（59 条映射）。
- **开发数据目录**：`.ramaria-dev/`（`cargo tauri dev` 时的数据根）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `lib.rs` | `run()` 装配（插件 / state / handler / setup）、`generate_handler!` 注册表 |
| `commands/` | chat / session / memory / config / mcp / export / index_cmd / import_cmd / persona / rules / keywords / style / evaluation / diagnostics / setup / dialog |
| `tray` / `notification` / `path_guard` / `events` | 宿主能力 |
| `frontend/js/api.js` | `RamariaApi` 封装（`_invoke()` → `TauriBridge.invoke()`） |

## 相邻契约

- 依赖：`ramaria-service`（含 `importer` feature）+ `ramaria-importer` + `ramaria-core`；Tauri 框架。
- **禁止**：记忆业务逻辑（一律经服务层用例）；`ramaria-service` 反向依赖本 crate。
- 契约面：command 名 / 参数 / 返回与契约基线逐项一致（61 项注册 / `api.js` 59 条封装 + 旁路调用，见基线 §2 / §3）。

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
