#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 测试债务盘点脚本。

职责:
- 扫描 crates 下 Rust 测试（独立测试文件 + #[cfg(test)] 内联段）与旧前端 JS 测试；
- 输出可量化的测试债务：规模、字面量夹具、golden 快照、调用次数断言、重复用例名、内联测试段、源码扫描文件；
- 供 M4-2 盘点与删除台账使用，只读，不修改任何文件。

用法:
- python scripts/check_test_debt.py
- python scripts/check_test_debt.py --root f:/Ramaria-s/main

退出码: 0 正常输出；2 用法 / 环境错误。
"""

import argparse
import io
import os
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Dict, List, NamedTuple, Optional, Tuple

# ===== 口径与常量 =====

FIXTURE_TYPES = ("MemoryEvent", "MemoryL1", "Persona", "UttBlock", "EventRelation")
INLINE_TEST_SUGGEST_LIMIT = 300
GOLDEN_REL = Path("crates/ramaria-service/tests/parity/golden")
JS_TESTS_REL = Path("crates/ramaria-desktop/frontend/tests")
EXTERNAL_TEST_MODULE = re.compile(
    r"^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;"
)
TEST_FN_RE = re.compile(
    r"#\[(?:tokio::)?test(?:\([^)]*\))?\]\s*"
    r"(?:#\[[^\]]*\]\s*)*"
    r"(?:pub(?:\s*\([^)]*\))?\s+)?"
    r"(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"
)
JS_TEST_NAME_RE = re.compile(r"test\(\s*['\"]([^'\"]+)['\"]")


# ===== 输出环境 =====


def reconfigure_streams() -> None:
    """在编码非 UTF-8 时把 stdout / stderr 切换到 UTF-8，失败则保持原状。"""
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


class SetupError(Exception):
    """用法或环境错误，统一触发退出码 2。"""


def read_text(path: Path) -> str:
    try:
        return io.open(path, encoding="utf-8").read()
    except OSError as exc:
        raise SetupError("无法读取 %s: %s" % (path, exc))


# ===== 扫描口径 =====


class RustFile(NamedTuple):
    """一个 Rust 源文件及其测试段。"""

    rel_path: str
    section: str  # test / inline / none
    lines: int
    test_lines: int
    test_text: str


def classify(rel_path: str) -> str:
    """按 test / production 判定文件类别（与 check_structure.py 同口径）。"""
    parts = rel_path.split("/")
    filename = parts[-1]
    if "tests" in parts[:-1]:
        return "test"
    if re.match(r".*tests.*\.rs$", filename) or re.match(r"^test_.*\.rs$", filename):
        return "test"
    return "production"


def inline_test_start(lines: List[str]) -> Optional[int]:
    """返回首个行首 #[cfg(test)] 的下标；其后为 `mod xxx;` 外置子模块时返回 None。"""
    for index, raw_line in enumerate(lines):
        cleaned = raw_line.rstrip("\r")
        if not cleaned.startswith("#[cfg(test)]"):
            continue
        rest = cleaned[len("#[cfg(test)]"):].strip()
        cursor = index + 1
        while not rest and cursor < len(lines):
            candidate = lines[cursor].strip()
            if candidate and not candidate.startswith("#[") and not candidate.startswith("//"):
                rest = candidate
                break
            cursor += 1
        if EXTERNAL_TEST_MODULE.match(rest):
            return None
        return index
    return None


def scan_rust(root: Path) -> List[RustFile]:
    """扫描 crates 下全部 .rs，返回测试段文本。"""
    crates_dir = root / "crates"
    if not crates_dir.is_dir():
        raise SetupError("root 下未找到 crates 目录: %s" % crates_dir)
    results: List[RustFile] = []
    for dirpath, dirnames, filenames in os.walk(str(crates_dir)):
        dirnames[:] = sorted(name for name in dirnames if name != "target")
        for filename in sorted(filenames):
            if not filename.endswith(".rs"):
                continue
            abs_path = Path(dirpath) / filename
            rel_path = os.path.relpath(str(abs_path), str(root)).replace(os.sep, "/")
            lines = read_text(abs_path).splitlines()
            if classify(rel_path) == "test":
                results.append(RustFile(rel_path, "test", len(lines), len(lines), "\n".join(lines)))
                continue
            start = inline_test_start(lines)
            if start is None:
                results.append(RustFile(rel_path, "none", len(lines), 0, ""))
                continue
            test_lines = len(lines) - start
            results.append(
                RustFile(rel_path, "inline", len(lines), test_lines, "\n".join(lines[start:]))
            )
    return results


def count_pattern(files: List[RustFile], pattern: re.Pattern) -> Tuple[int, int]:
    """返回（命中次数，命中文件数）。"""
    hits = 0
    matched_files = 0
    for item in files:
        if not item.test_text:
            continue
        found = len(pattern.findall(item.test_text))
        if found:
            hits += found
            matched_files += 1
    return hits, matched_files


def _is_return_type(text: str, start: int) -> bool:
    """判断匹配点是否位于函数返回类型位置（箭头后 path::T 的花括号属于函数体）。"""
    window = text[max(0, start - 160) : start]
    boundary = max(window.rfind(";"), window.rfind("{"), window.rfind("}"))
    return "->" in window[boundary + 1 :]


def count_struct_literals(files: List[RustFile], name: str) -> Tuple[int, int]:
    """返回（字面量次数，命中文件数），排除函数返回类型 `-> T {` 的花括号。"""
    pattern = re.compile(r"\b%s\s*\{" % re.escape(name))
    hits = 0
    matched_files = 0
    for item in files:
        if not item.test_text:
            continue
        found = 0
        for match in pattern.finditer(item.test_text):
            if _is_return_type(item.test_text, match.start()):
                continue
            found += 1
        if found:
            hits += found
            matched_files += 1
    return hits, matched_files


def extract_test_names(files: List[RustFile]) -> Counter:
    names: Counter = Counter()
    for item in files:
        if item.test_text:
            names.update(TEST_FN_RE.findall(item.test_text))
    return names


def scan_js_tests(root: Path) -> List[Tuple[str, int, bool]]:
    """返回旧前端 JS 测试文件（相对路径，行数，是否源码扫描）。"""
    tests_dir = root / JS_TESTS_REL
    if not tests_dir.is_dir():
        return []
    results = []
    for abs_path in sorted(tests_dir.rglob("*.js")):
        text = read_text(abs_path)
        rel_path = os.path.relpath(str(abs_path), str(root)).replace(os.sep, "/")
        source_scan = re.search(
            r"readFileSync\s*\(|vm\.runInNewContext|new\s+Function\s*\(", text
        ) is not None
        results.append((rel_path, len(text.splitlines()), source_scan))
    return results


# ===== 输出 =====


def render(root: Path) -> None:
    rust_files = scan_rust(root)
    rust_tests = [item for item in rust_files if item.section == "test"]
    rust_inline = [item for item in rust_files if item.section == "inline"]
    js_tests = scan_js_tests(root)

    print("== 测试规模 ==")
    print(
        "Rust 测试文件 %d（独立 %d / 内联 %d），测试代码行 %d"
        % (
            len(rust_tests) + len(rust_inline),
            len(rust_tests),
            len(rust_inline),
            sum(item.test_lines for item in rust_tests + rust_inline),
        )
    )
    print("旧前端 JS 测试 %d 文件 / %d 行" % (len(js_tests), sum(item[1] for item in js_tests)))
    golden = sorted((root / GOLDEN_REL).glob("*.json")) if (root / GOLDEN_REL).is_dir() else []
    print("golden 快照 %d 份" % len(golden))

    print("\n== 字面量夹具（测试段内） ==")
    for name in FIXTURE_TYPES:
        hits, files = count_struct_literals(rust_files, name)
        print("%-14s %4d 处 / %3d 文件" % (name, hits, files))

    print("\n== 可删 / 可并候选 ==")
    hits, files = count_pattern(rust_files, re.compile(r"\bcall_count\b|\.times\s*\("))
    print("调用次数断言        %4d 处 / %3d 文件" % (hits, files))

    names = extract_test_names(rust_files)
    duplicates = sorted(((name, count) for name, count in names.items() if count > 1),
                        key=lambda item: (-item[1], item[0]))
    print("重复用例名          %4d 个（同名 ≥2 次）" % len(duplicates))
    for name, count in duplicates[:10]:
        print("  - %s × %d" % (name, count))

    inline_over = [item for item in rust_inline if item.test_lines > INLINE_TEST_SUGGEST_LIMIT]
    print("内联测试段 >%d 行   %4d 文件" % (INLINE_TEST_SUGGEST_LIMIT, len(inline_over)))
    for item in inline_over[:10]:
        print("  - %s（%d 行）" % (item.rel_path, item.test_lines))

    source_scan = [item for item in js_tests if item[2]]
    print("旧前端源码扫描      %4d 文件 / %d 行" % (len(source_scan), sum(item[1] for item in source_scan)))
    for rel_path, lines, _ in source_scan[:10]:
        print("  - %s（%d 行）" % (rel_path, lines))


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="Ramaria 测试债务盘点：统计夹具、golden、重复用例与源码扫描测试。"
    )
    parser.add_argument("--root", default=None, help="仓库根目录（默认取脚本所在目录的父目录）")
    args = parser.parse_args(argv)

    reconfigure_streams()
    script_dir = Path(__file__).resolve().parent
    root = Path(args.root).resolve() if args.root else script_dir.parent
    if not root.is_dir():
        print("[错误] root 目录不存在: %s" % root, file=sys.stderr)
        return 2
    try:
        render(root)
    except SetupError as exc:
        print("[错误] %s" % exc, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
