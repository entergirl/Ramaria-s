#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 前端结构校验脚本。

职责:
- tokens.css 的变量定义覆盖全部 var(--x) 引用；
- 路由声明的组件文件存在，路由 name 唯一；
- src/views（含子目录）至少存在契约清单中的视图文件；
- UI 规范：生产源码无裸色值 / 无内联 style / 无 999px（导航圆键除外）/ 无外部资源；
- 生产 CSP 无 `unsafe-inline` 与 `unsafe-eval`。
- 不一致即退出码 1。

用法: python scripts/check_frontend.py
退出码: 0 一致；1 不一致；2 用法 / 环境错误。
"""

import argparse
import io
import re
import sys
from pathlib import Path
from typing import Dict, List, Set

FRONTEND_DIR = Path("crates/ramaria-desktop/frontend")
CONTRACT_BASELINE = Path("scripts/contract-baseline-3.0.json")
DESKTOP_DIR = Path("crates/ramaria-desktop")

# UI 规范：色值只能经令牌引用；令牌文件本身是调色板，不参与裸色值检查
TOKEN_FILE = "styles/tokens.css"
NAV_PREFIX = "components/nav"
RAW_COLOR = re.compile(r"#[0-9a-fA-F]{3,8}\b|\b(?:rgba?|hsla?)\(")
INLINE_STYLE = re.compile(r"(?:\sstyle=|\s:style=|\sv-bind:style=)")
ROUND_PILL = re.compile(r"\b999px\b")
REMOTE_ASSET = re.compile(r"fonts\.googleapis|cdn\.|unpkg\.com|jsdelivr")
CSP_UNSAFE = ("unsafe-inline", "unsafe-eval")


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


def load_expected_views(root: Path) -> List[str]:
    import json

    try:
        manifest = json.loads(read_text(root / CONTRACT_BASELINE))
    except ValueError as exc:
        raise SetupError("契约基线 JSON 解析失败: %s" % exc)
    return list(manifest.get("views", []))


def collect_css_vars(files: List[Path]) -> Dict[str, Set[str]]:
    definitions: Set[str] = set()
    uses: Set[str] = set()
    for path in files:
        text = read_text(path)
        definitions |= set(re.findall(r"(--[a-zA-Z0-9-]+)\s*:", text))
        # 仅统计无回退写法：var(--x, #fff) 自带兜底，不视为缺失
        uses |= set(re.findall(r"var\(\s*(--[a-zA-Z0-9-]+)\s*\)", text))
    return {"definitions": definitions, "uses": uses}


def check_frontend(root: Path, expected_views: List[str]) -> List[str]:
    failures: List[str] = []
    base = root / FRONTEND_DIR
    src = base / "src"
    if not src.is_dir():
        failures.append("frontend/src 不存在")
        return failures

    css_files = sorted(list(src.glob("**/*.css")) + list(src.glob("**/*.vue")))
    variables = collect_css_vars(css_files)
    missing = sorted(variables["uses"] - variables["definitions"])
    if missing:
        failures.append("CSS / Vue 引用了未定义的变量: %s" % ", ".join(missing))

    router_file = src / "app" / "router.ts"
    if not router_file.is_file():
        failures.append("路由文件不存在: src/app/router.ts")
    router_files = [router_file] if router_file.is_file() else []
    route_names: List[str] = []
    for path in router_files:
        text = read_text(path)
        # 重定向目标与守卫返回值都不是路由声明：先剔除 redirect 块，再按 path 取段内首个 name
        text = re.sub(r"redirect\s*:\s*\{[^}]*\}", "", text)
        for segment in re.split(r"\bpath\s*:", text)[1:]:
            match = re.search(r"\bname\s*:\s*['\"]([^'\"]+)['\"]", segment)
            if match:
                route_names.append(match.group(1))
        for spec in re.findall(r"import\(\s*['\"]([^'\"]+\.vue)['\"]\s*\)", text):
            target = (path.parent / spec).resolve()
            if not target.exists():
                failures.append(
                    "路由组件不存在: %s -> %s" % (path.relative_to(root).as_posix(), spec)
                )
    duplicates = sorted({name for name in route_names if route_names.count(name) > 1})
    if duplicates:
        failures.append("路由 name 重复: %s" % ", ".join(duplicates))

    views_dir = src / "views"
    view_files: Set[str] = set()
    for path in views_dir.rglob("*.vue"):
        relative = path.relative_to(views_dir)
        # 目录式视图（views/chat/index.vue）取首段目录名；平铺文件取文件名
        view_files.add(relative.parts[0] if len(relative.parts) > 1 else path.stem)
    if view_files:
        missing_views = sorted(set(expected_views) - view_files)
        if missing_views:
            failures.append("src/views 缺少契约视图文件: %s" % ", ".join(missing_views))
    return failures


def check_frontend_ui(root: Path) -> List[str]:
    """UI 规范扫描：裸色值、内联样式、999px 与外部资源。"""
    failures: List[str] = []
    src = root / FRONTEND_DIR / "src"
    if not src.is_dir():
        return failures

    for path in sorted(list(src.glob("**/*.vue")) + list(src.glob("**/*.css"))):
        relative = path.relative_to(src).as_posix()
        is_token = relative == TOKEN_FILE
        is_nav = relative.startswith(NAV_PREFIX)
        for number, line in enumerate(read_text(path).splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith(("/*", "*", "//", "<!--")):
                continue
            if not is_token and RAW_COLOR.search(line):
                failures.append("%s:%d 裸色值，请改用语义令牌" % (relative, number))
            if INLINE_STYLE.search(line):
                failures.append("%s:%d 内联样式，请改用 class 与 CSSOM 变量" % (relative, number))
            if not is_nav and ROUND_PILL.search(line):
                failures.append("%s:%d 出现 999px，圆角请按分档令牌取值" % (relative, number))
            if REMOTE_ASSET.search(line):
                failures.append("%s:%d 引用了外部资源" % (relative, number))
    return failures


def load_security(path: Path, label: str) -> Dict[str, object]:
    import json

    try:
        manifest = json.loads(read_text(path))
    except ValueError as exc:
        raise SetupError("%s 解析失败: %s" % (label, exc))
    security = manifest.get("app", {}).get("security", {})
    return security if isinstance(security, dict) else {}


def check_csp(root: Path) -> List[str]:
    """生产 CSP：必须存在且不含 unsafe-inline / unsafe-eval。"""
    failures: List[str] = []

    main_conf = root / DESKTOP_DIR / "tauri.conf.json"
    if not main_conf.is_file():
        failures.append("tauri.conf.json 不存在")
        return failures
    csp = load_security(main_conf, "tauri.conf.json").get("csp")
    if not csp:
        failures.append("tauri.conf.json 缺少生产 CSP")
    else:
        for token in CSP_UNSAFE:
            if token in str(csp):
                failures.append("tauri.conf.json 的生产 CSP 含 %s" % token)
    return failures


def main(argv: List[str]) -> int:
    reconfigure_streams()
    parser = argparse.ArgumentParser(description="校验 Ramaria 前端结构")
    parser.add_argument("--root", default=None, help="main 仓库根目录（默认脚本上一级）")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    try:
        expected_views = load_expected_views(root)
        failures = check_frontend(root, expected_views)
        failures += check_frontend_ui(root)
        failures += check_csp(root)
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    if failures:
        print("[FAIL] 前端结构不一致:")
        for failure in failures:
            print("  - %s" % failure)
        return 1

    print("[OK] 前端结构一致（视图 %d）" % len(expected_views))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
