#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 前端结构校验脚本（旧前端过渡期 + frontend-v3 落地后）。

职责:
- 过渡期（frontend-v3 不存在）：
  * index.html 的视图容器为 10 个且与契约清单一致；
  * CSS 变量引用（无回退写法）必须在 CSS 中有定义，历史欠账登记在 LEGACY_TOKEN_DEBT；
- frontend-v3 存在（M5 起）：
  * tokens.css 的变量定义覆盖全部 var(--x) 引用；
  * 路由声明的组件文件存在，路由 name 唯一；
  * src/views（含子目录）至少存在契约清单中的视图文件；
  * UI 规范：生产源码无裸色值 / 无内联 style / 无 999px（导航圆键除外）/ 无外部资源；
  * 生产 CSP 无 `unsafe-inline` 与 `unsafe-eval`。
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

LEGACY_DIR = Path("crates/ramaria-desktop/frontend")
V3_DIR = Path("crates/ramaria-desktop/frontend-v3")
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

# 旧前端历史欠账：引用了但从未定义的 CSS 变量（随旧前端在 M7 删除）。
# 只允许减少，不允许新增；新增未定义令牌会使本脚本失败。
LEGACY_TOKEN_DEBT = {"--bg-muted", "--bg-surface", "--blue-900"}


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


def check_legacy(root: Path, expected_views: List[str]) -> List[str]:
    failures: List[str] = []
    base = root / LEGACY_DIR
    if not base.is_dir():
        failures.append("旧前端目录不存在: %s" % LEGACY_DIR.as_posix())
        return failures

    index = read_text(base / "index.html")
    views = re.findall(r'<div class="view(?: [^"]*)?" id="view-([a-z-]+)"', index)
    if set(views) != set(expected_views) or len(views) != len(expected_views):
        failures.append(
            "旧前端视图容器: 实际 %s / 期望 %s" % (sorted(views), sorted(expected_views))
        )

    css_files = sorted(base.glob("**/*.css"))
    variables = collect_css_vars(css_files)
    missing = sorted(variables["uses"] - variables["definitions"])
    new_missing = sorted(set(missing) - LEGACY_TOKEN_DEBT)
    if new_missing:
        failures.append("CSS 引用了未定义的变量: %s" % ", ".join(new_missing))
    stale_debt = sorted(LEGACY_TOKEN_DEBT - set(missing))
    if stale_debt:
        failures.append(
            "LEGACY_TOKEN_DEBT 中的变量已不再缺失，请从清单移除: %s"
            % ", ".join(stale_debt)
        )
    return failures


def check_v3(root: Path, expected_views: List[str]) -> List[str]:
    failures: List[str] = []
    base = root / V3_DIR
    src = base / "src"
    if not src.is_dir():
        failures.append("frontend-v3/src 不存在")
        return failures

    css_files = sorted(list(src.glob("**/*.css")) + list(src.glob("**/*.vue")))
    variables = collect_css_vars(css_files)
    missing = sorted(variables["uses"] - variables["definitions"])
    if missing:
        failures.append("CSS / Vue 引用了未定义的变量: %s" % ", ".join(missing))

    router_files = sorted(src.glob("router/**/*.ts")) + sorted(src.glob("router/*.ts"))
    route_names: List[str] = []
    for path in router_files:
        text = read_text(path)
        if re.search(r"\bpath\s*:", text):
            route_names.extend(re.findall(r"\bname\s*:\s*['\"]([^'\"]+)['\"]", text))
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


def check_v3_ui(root: Path) -> List[str]:
    """UI 规范扫描：裸色值、内联样式、999px 与外部资源。"""
    failures: List[str] = []
    src = root / V3_DIR / "src"
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
    if main_conf.is_file():
        csp = load_security(main_conf, "tauri.conf.json").get("csp")
        if not csp:
            failures.append("tauri.conf.json 缺少生产 CSP")
        else:
            for token in CSP_UNSAFE:
                if token in str(csp):
                    failures.append("tauri.conf.json 的生产 CSP 含 %s" % token)

    overlay = root / DESKTOP_DIR / "tauri.v3.conf.json"
    if overlay.is_file():
        csp = load_security(overlay, "tauri.v3.conf.json").get("csp")
        if csp:
            for token in CSP_UNSAFE:
                if token in str(csp):
                    failures.append("tauri.v3.conf.json 的生产 CSP 含 %s" % token)
    return failures


def main(argv: List[str]) -> int:
    reconfigure_streams()
    parser = argparse.ArgumentParser(description="校验 Ramaria 前端结构")
    parser.add_argument("--root", default=None, help="main 仓库根目录（默认脚本上一级）")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    try:
        expected_views = load_expected_views(root)
        if (root / V3_DIR).is_dir():
            failures = check_v3(root, expected_views)
            failures += check_v3_ui(root)
            failures += check_csp(root)
            mode = "frontend-v3"
        else:
            failures = check_legacy(root, expected_views)
            mode = "旧前端（frontend-v3 未创建）"
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    if failures:
        print("[FAIL] 前端结构不一致（%s）:" % mode)
        for failure in failures:
            print("  - %s" % failure)
        return 1

    print("[OK] 前端结构一致（%s；视图 %d）" % (mode, len(expected_views)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
