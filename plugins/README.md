# Ramaria 插件目录

> 状态：v1.0 预留，**当前不开发**  
> 激活版本：v1.1+

## 目录说明

| 子目录 | 用途 | 状态 |
|---|---|---|
| `llm/` | LLM 后端插件（自定义 provider） | 预留 |
| `embedding/` | Embedding 模型后端插件 | 预留 |
| `push/` | 推送通道插件（Telegram/邮件等） | 预留 |
| `export/` | 导出格式插件 | 预留 |

## v1.0 策略

v1.0 只做**编译期 feature + trait crate**，不做运行时动态加载。
所有插件功能通过 `Cargo.toml` 的 `[features]` 控制编译。

## v1.1+ 规划

- 支持运行时 `.dll` / `.so` / `.dylib` 动态加载
- 每个子目录下定义对应的 trait crate
- 第三方可基于 trait 开发自定义插件
