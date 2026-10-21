#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 入口契约一致性校验脚本。

职责:
- 解析 Rust 源码的机器事实（Tauri 注册表 / CLI 顶层枚举 / MCP 工具 / 桌面事件 / migration / 视图容器）；
- 解析旧前端 api.js 的 _invoke 调用点；
- 与 scripts/contract-baseline-3.0.json（docs/dev-3.0/test/contract-baseline-3.0.md 的机器投影）逐项比对；
- 任一集合或计数不一致即退出码 1（秒级，供 CI 与本地使用）。

用法:
- python scripts/check_contract.py
- python scripts/check_contract.py --root f:/Ramaria-s/main

退出码: 0 一致；1 不一致；2 用法 / 环境错误。
"""

import argparse
import io
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Set

MANIFEST_REL = Path("scripts/contract-baseline-3.0.json")
DESKTOP_LIB = Path("crates/ramaria-desktop/src/lib.rs")
FRONTEND_API = Path("crates/ramaria-desktop/frontend/js/api.js")
FRONTEND_INDEX = Path("crates/ramaria-desktop/frontend/index.html")
CLI_LIB = Path("crates/ramaria-cli/src/cli.rs")
MCP_TOOLS_DIR = Path("crates/ramaria-mcp/src/tools")
EVENTS_FILE = Path("crates/ramaria-desktop/src/events.rs")
MIGRATION_DIR = Path("crates/ramaria-storage/migrations")


class SetupError(Exception):
    """用法或环境错误，统一触发退出码 2。"""


def reconfigure_streams() -> None:
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is None:
            continue
        encoding = (getattr(stream, "encoding", None) or "").lower()
        if "utf-8" in encoding:
            continue
        try:
            reconfigure(encoding="utf-8", errors="replace")
        except (ValueError, OSError):
            pass


def read_text(path: Path) -> str:
    try:
        return io.open(path, encoding="utf-8").read()
    except OSError as exc:
        raise SetupError("无法读取 %s: %s" % (path, exc))


def load_manifest(root: Path) -> Dict:
    path = root / MANIFEST_REL
    try:
        return json.loads(read_text(path))
    except ValueError as exc:
        raise SetupError("契约基线 JSON 解析失败: %s" % exc)


def parse_registry(root: Path) -> List[str]:
    """解析 generate_handler![...] 内的 commands::x 与 tray::x（逐项点名）。"""
    text = read_text(root / DESKTOP_LIB)
    start = text.find("generate_handler![")
    if start < 0:
        raise SetupError("未找到 generate_handler! 注册表")
    names: List[str] = []
    for line in text[start:].splitlines()[1:]:
        code = line.split("//")[0].strip().rstrip(",")
        if code.startswith("])"):
            break
        if code.startswith("commands::") or code.startswith("tray::"):
            names.append(code.split("::")[-1])
    return names


def parse_frontend_calls(root: Path) -> List[str]:
    text = read_text(root / FRONTEND_API)
    return re.findall(r"_invoke\(\s*['\"]([a-z_][a-z0-9_]*)['\"]", text)


def parse_cli_top(root: Path) -> List[str]:
    text = read_text(root / CLI_LIB)
    start = text.find("pub(crate) enum Commands {")
    if start < 0:
        start = text.find("enum Commands {")
    if start < 0:
        raise SetupError("未找到 Commands 枚举")
    end = text.find("\n}", start)
    if end < 0:
        raise SetupError("Commands 枚举缺少闭合括号")
    body = text[start:end]
    names: List[str] = []
    for line in body.splitlines():
        match = re.match(r"^    ([A-Z][A-Za-z0-9_]*)\s*(?:\{|,|\(|$)", line)
        if match:
            names.append(match.group(1))
    return names


def parse_mcp_tools(root: Path) -> List[str]:
    names: List[str] = []
    for path in sorted((root / MCP_TOOLS_DIR).glob("*.rs")):
        text = read_text(path)
        for match in re.finditer(
            r"#\[rmcp::tool\(([\s\S]*?)\]\s*\n\s*async fn (\w+)", text
        ):
            names.append(match.group(2))
    return names


def parse_events(root: Path) -> List[str]:
    text = read_text(root / EVENTS_FILE)
    return re.findall(r'pub const EVENT_[A-Z0-9_]+: &str = "([^"]+)";', text)


def parse_views(root: Path) -> List[str]:
    text = read_text(root / FRONTEND_INDEX)
    return re.findall(r'<div class="view(?: [^"]*)?" id="view-([a-z-]+)"', text)


def compare_list(
    label: str, actual: List[str], expected: List[str], failures: List[str]
) -> None:
    """按集合比对；条目重复也视为不一致（注册表 / 枚举 / 事件名不允许重复）。"""
    actual_set = set(actual)
    expected_set = set(expected)
    missing = sorted(expected_set - actual_set)
    extra = sorted(actual_set - expected_set)
    duplicated = sorted({name for name in actual if actual.count(name) > 1})
    if not missing and not extra and not duplicated:
        return
    detail = "实际 %d / 期望 %d" % (len(actual), len(expected))
    if duplicated:
        detail += "；重复: %s" % ", ".join(duplicated)
    if missing:
        detail += "；缺少: %s" % ", ".join(missing)
    if extra:
        detail += "；多出: %s" % ", ".join(extra)
    failures.append("%s: %s" % (label, detail))


def main(argv: List[str]) -> int:
    reconfigure_streams()
    parser = argparse.ArgumentParser(description="校验 Ramaria 3.0 入口契约")
    parser.add_argument("--root", default=None, help="main 仓库根目录（默认脚本上一级）")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    try:
        manifest = load_manifest(root)
        registry = parse_registry(root)
        frontend_calls = parse_frontend_calls(root)
        cli_top = parse_cli_top(root)
        mcp_tools = parse_mcp_tools(root)
        events = parse_events(root)
        views = parse_views(root)
        migrations = sorted(path.name for path in (root / MIGRATION_DIR).glob("*.sql"))
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    failures: List[str] = []

    compare_list("Tauri 注册表", registry, manifest["commands"], failures)
    compare_list("CLI 顶层子命令", cli_top, manifest["cli_top"], failures)
    compare_list("MCP 工具", mcp_tools, manifest["mcp_tools"], failures)
    compare_list("桌面事件", events, manifest["events"], failures)
    compare_list("前端视图容器", views, manifest["views"], failures)
    compare_list("migration 文件", migrations, manifest["migration_files"], failures)

    # 前端调用面：计数 + 子集 + 未封装集合
    calls_set = set(frontend_calls)
    registry_set = set(registry)
    if len(frontend_calls) != manifest["frontend_calls"]:
        failures.append(
            "api.js 调用点: 实际 %d / 期望 %d"
            % (len(frontend_calls), manifest["frontend_calls"])
        )
    unknown = sorted(calls_set - registry_set)
    if unknown:
        failures.append("api.js 调用了未注册命令: %s" % ", ".join(unknown))
    unwrapped = sorted(registry_set - calls_set)
    expected_unwrapped = sorted(manifest["unwrapped_commands"])
    if unwrapped != expected_unwrapped:
        failures.append(
            "未封装命令: 实际 %s / 期望 %s" % (unwrapped, expected_unwrapped)
        )
    # 未被 api.js 调用的命令必须全部在登记清单内，反向也成立（上面已双向比对）

    if failures:
        print("[FAIL] 契约面不一致（基线: %s）:" % manifest.get("source_doc", "?"))
        for failure in failures:
            print("  - %s" % failure)
        return 1

    print(
        "[OK] 契约面一致：命令 %d / 前端调用 %d / CLI %d / MCP %d / 事件 %d / 视图 %d / migration %d"
        % (
            len(registry),
            len(frontend_calls),
            len(cli_top),
            len(mcp_tools),
            len(events),
            len(views),
            len(migrations),
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
