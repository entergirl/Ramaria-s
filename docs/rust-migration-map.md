# Ramaria Python 到 Rust 迁移映射

> 状态：初始化
> 适用范围：Rust 重构期间的模块迁移追踪

本文用于记录 Python 现有模块到 Rust workspace 的迁移目标、状态和测试覆盖。

## 状态约定

| 状态 | 含义 |
|---|---|
| `todo` | 尚未开始 |
| `in-progress` | 正在迁移或 POC |
| `ported` | 代码已迁移 |
| `tested` | 已有对应测试并通过 |
| `deferred` | 明确延后 |

## 初始映射

| Python 模块 | Rust 目标 | 状态 | 测试/验收 |
|---|---|---|---|
| `src/ramaria/config.py` | `crates/ramaria-core` | `todo` | 配置类型、默认值、环境覆盖 |
| `src/ramaria/database.py` | `crates/ramaria-storage` | `todo` | SQLite schema、migration、CRUD |
| `src/ramaria/memory*.py` | `crates/ramaria-memory` | `todo` | L1/L2/L3、衰减、图谱、冲突检测 |
| `src/ramaria/llm*.py` | `crates/ramaria-llm` | `todo` | OpenAI-compatible streaming、provider smoke test |
| `src/ramaria/api*.py` | `crates/ramaria-app` | `todo` | 应用用例编排、错误映射 |
| CLI 入口 | `crates/ramaria-cli` | `todo` | clap 命令、REPL、配置向导 |
| 桌面入口 | `crates/ramaria-desktop` | `todo` | Tauri commands、events、托盘、通知 |

## 记录规则

- 每迁移一个模块，补充来源文件、目标 crate、状态和测试命令。
- 发现旧逻辑不完整或需要产品决策时，先记录为 `deferred`，再补 ADR 或任务编号。
- 不在 Rust v1.0 中兼容旧 Python 数据库 schema；映射只用于逻辑迁移参考。
