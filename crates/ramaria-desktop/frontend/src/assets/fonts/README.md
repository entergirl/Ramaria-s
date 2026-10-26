# Ramaria 自托管字体

6 个 TTF 文件 ≈ 600KB，一次性下载后提交 Git，构建期零网络依赖。

## 文件清单

| 文件 | 字体 | 类型 |
|------|------|------|
| dm-sans.ttf | DM Sans 300/400/500/600 | 可变字体 |
| dm-serif-display.ttf | DM Serif Display Regular | 静态 |
| dm-serif-display-italic.ttf | DM Serif Display Italic | 静态 |
| jetbrains-mono-400.ttf | JetBrains Mono Regular | 静态 |
| jetbrains-mono-500.ttf | JetBrains Mono Medium | 静态 |

## 如需重新下载

DM Sans 可变字体（替代旧的 4 个静态文件）：

```
https://raw.githubusercontent.com/google/fonts/main/ofl/dmsans/DMSans%5Bopsz%2Cwght%5D.ttf
```

另存为 `dm-sans.ttf`。其余 4 个文件从各自官方仓库下载，不再赘述。

## 系统字体回退

| 用途 | 首选 | 回退 |
|------|------|------|
| 展示/标题 | DM Serif Display | Georgia, Times New Roman |
| 正文/UI | DM Sans | -apple-system, PingFang SC, Microsoft YaHei |
| 代码/数据 | JetBrains Mono | Fira Code, Cascadia Code, Consolas |
