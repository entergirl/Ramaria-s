# Ramaria 自托管字体（自动下载，推荐提交到 Git）

字体文件由 `build.rs` 在 `cargo build` / `cargo tauri dev` 时自动下载。
**无需手动操作。**

## 自动下载机制

- **触发时机**: 每次 `cargo build`（仅首次下载，已存在则跳过）
- **主 CDN**: jsDelivr（中国大陆有节点，国内访问速度快）
- **备用 CDN**: GitHub raw（海外用户 / jsDelivr 不可用时）
- **失败处理**: 双 CDN 均失败时输出 warning，应用使用系统字体正常运行
- **缓存策略**: 文件存在即跳过，不重复下载

## 推荐：提交字体文件到 Git

8 个 TTF 文件合计约 400KB，建议直接提交到仓库：

```bash
git add fonts/*.ttf
git commit -m "chore: 添加自托管字体文件"
```

提交后优点：
- 任何 clone 仓库的人立即可用，无需网络下载
- CI/CD 构建不受网络影响
- 中国大陆用户无任何网络问题
- `tauri build` 打包时字体 100% 可用

## 系统字体回退

即使字体全部缺失，应用仍正常运行：

| 用途 | 首选字体 | 回退系统字体 |
|------|---------|-------------|
| 展示/标题 | DM Serif Display | Georgia, Times New Roman |
| 正文/UI | DM Sans | -apple-system, PingFang SC, Microsoft YaHei |
| 代码/数据 | JetBrains Mono | Fira Code, Cascadia Code, Consolas |

## 强制重新下载

```powershell
Remove-Item fonts\*.ttf
cargo build -p ramaria-desktop
```
