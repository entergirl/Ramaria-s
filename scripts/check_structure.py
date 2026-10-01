#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 结构红线自检脚本。

扫描 <root>/crates 下全部 .rs 文件，按文件类别对照行数红线，输出阻断 / 建议 / 例外清单。

文件分类与红线（按序判定）:
- barrel: 文件名 mod.rs / lib.rs，红线 500 行
- test: 路径含名为 tests 的目录段，或文件名匹配 *tests*.rs / test_*.rs，红线 1200 行
- production: 其余文件，红线 700 行，建议线 500 行
- production 文件中，首个行首无缩进的 #[cfg(test)] 起至文件末尾超过 300 行，记为建议项
  （其后为 `mod xxx;` 外置子模块声明、测试已迁出本文件时不计）

例外清单（脚本同目录 structure-exemptions.toml）:
- 条目字段 path / reason / date；path 为相对 main/ 的 posix 路径，支持 fnmatch 通配
- 命中的超标文件不计阻断，改列"例外（登记）"节
- 清单缺失视为无例外；解析失败或条目缺字段退出码 2
- 文件不存在或已不超标的条目列为过期例外（警告，不阻断）

TOML 解析:
- 优先标准库 tomllib（Python 3.11+ 提供）；不可用时回退到内置受限子集解析器
- 受限子集仅支持注释行、[[exemption]] 表头、key = "value" 字符串

退出码:
- 0: 无阻断项；1: 存在阻断项；2: 用法或环境错误（root 不存在、例外清单非法等）
"""

import argparse
import fnmatch
import os
import re
import sys
from pathlib import Path
from typing import Dict, List, NamedTuple, Optional, Tuple

# ===== 阈值与常量 =====

LIMITS = {
    "barrel": 500,
    "test": 1200,
    "production": 700,
}
PRODUCTION_SUGGEST_LIMIT = 500
INLINE_TEST_SUGGEST_LIMIT = 300

EXEMPTION_FILENAME = "structure-exemptions.toml"
EXEMPTION_FIELDS = ("path", "reason", "date")


# ===== 数据结构 =====


class SetupError(Exception):
    """用法或环境错误，统一触发退出码 2。"""


class ExemptionEntry(NamedTuple):
    """一条例外登记：pattern 为相对 main/ 的 posix 路径或 fnmatch 通配。"""

    pattern: str
    reason: str
    date: str


class SourceFile(NamedTuple):
    """一个被扫描的 Rust 源文件及其度量结果。"""

    rel_path: str
    category: str
    lines: int
    inline_test_lines: Optional[int]


# ===== 输出环境 =====


def reconfigure_streams() -> None:
    """在编码非 UTF-8 时把 stdout / stderr 切换到 UTF-8，失败则保持原状（兼容旧版本）。"""
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


# ===== 文件扫描与度量 =====


def read_source(abs_path: Path) -> Tuple[int, List[str]]:
    """读取源文件，返回 (总行数, 按换行切分的行列表)。

    总行数按换行符个数统计，末尾无换行的残留行计为一行，与常见编辑器行数一致。
    """
    try:
        data = abs_path.read_bytes()
    except OSError as exc:
        raise SetupError("无法读取文件 %s: %s" % (abs_path, exc))
    total = data.count(b"\n")
    if data and not data.endswith(b"\n"):
        total += 1
    text = data.decode("utf-8", errors="replace")
    return total, text.split("\n")


def classify(rel_path: str) -> str:
    """按 barrel / test / production 的判定顺序返回文件类别。"""
    parts = rel_path.split("/")
    filename = parts[-1]
    if filename in ("mod.rs", "lib.rs"):
        return "barrel"
    if "tests" in parts[:-1]:
        return "test"
    if fnmatch.fnmatchcase(filename, "*tests*.rs"):
        return "test"
    if fnmatch.fnmatchcase(filename, "test_*.rs"):
        return "test"
    return "production"


def measure_inline_test(lines: List[str], total: int) -> Optional[int]:
    """返回首个行首无缩进的 #[cfg(test)] 起至文件末尾的行数；未找到返回 None。

    #[cfg(test)] 后若为 `mod xxx;` 外置子模块声明（测试位于同级 tests 文件），
    不计为内联测试段。
    """
    for index, raw_line in enumerate(lines):
        cleaned = raw_line.rstrip("\r")
        if not cleaned.startswith("#[cfg(test)]"):
            continue
        if is_external_test_module(lines, index, cleaned):
            return None
        return total - index
    return None


def is_external_test_module(lines: List[str], index: int, cfg_line: str) -> bool:
    """判断 #[cfg(test)] 之后的条目是否为 `mod xxx;` 外置子模块声明。"""
    rest = cfg_line[len("#[cfg(test)]"):].strip()
    cursor = index + 1
    while not rest and cursor < len(lines):
        candidate = lines[cursor].strip()
        if candidate and not candidate.startswith("#[") and not candidate.startswith("//"):
            rest = candidate
            break
        cursor += 1
    pattern = r"^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;"
    return re.match(pattern, rest) is not None


def scan_crates(root: Path) -> List[SourceFile]:
    """递归扫描 <root>/crates 下全部 .rs 文件，排除名为 target 的目录段。"""
    crates_dir = root / "crates"
    if not crates_dir.is_dir():
        raise SetupError("root 下未找到 crates 目录: %s" % crates_dir)
    results = []
    for dirpath, dirnames, filenames in os.walk(str(crates_dir)):
        dirnames[:] = sorted(name for name in dirnames if name != "target")
        for filename in sorted(filenames):
            if not filename.endswith(".rs"):
                continue
            abs_path = Path(dirpath) / filename
            rel_path = os.path.relpath(str(abs_path), str(root)).replace(os.sep, "/")
            category = classify(rel_path)
            lines, text_lines = read_source(abs_path)
            inline = measure_inline_test(text_lines, lines) if category == "production" else None
            results.append(SourceFile(rel_path, category, lines, inline))
    results.sort(key=lambda item: item.rel_path)
    return results


# ===== 例外清单解析 =====


def parse_restricted_toml(text: str) -> Dict[str, list]:
    """受限 TOML 子集解析器：仅支持注释行、[[exemption]] 表头、key = "value" 字符串。"""
    entries = []
    current = None
    for lineno, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line == "[[exemption]]":
            current = {}
            entries.append(current)
            continue
        match = re.match(r'^([A-Za-z_][A-Za-z0-9_-]*)\s*=\s*"(.*)"$', line)
        if match is None:
            raise SetupError(
                '第 %d 行无法解析，受限子集仅支持注释行、[[exemption]] 表头与 key = "value" 字符串: %s'
                % (lineno, line[:80])
            )
        if current is None:
            raise SetupError("第 %d 行出现键值，但此前没有 [[exemption]] 表头" % lineno)
        key, value = match.group(1), match.group(2)
        if key not in EXEMPTION_FIELDS:
            raise SetupError("第 %d 行键 %r 不在支持范围（%s）内" % (lineno, key, ", ".join(EXEMPTION_FIELDS)))
        if "\\" in value:
            raise SetupError("第 %d 行字符串含反斜杠，受限子集不支持转义" % lineno)
        if key in current:
            raise SetupError("第 %d 行键 %r 重复" % (lineno, key))
        current[key] = value
    return {"exemption": entries}


def extract_entries(data: dict, path: Path) -> List[ExemptionEntry]:
    """校验例外条目字段并返回 ExemptionEntry 列表。"""
    raw = data.get("exemption", [])
    if not isinstance(raw, list):
        raise SetupError("例外清单 %s 中 exemption 必须是表数组" % path)
    entries = []
    for index, item in enumerate(raw, 1):
        if not isinstance(item, dict):
            raise SetupError("例外清单 %s 第 %d 条必须是 [[exemption]] 表" % (path, index))
        values = {}
        for field in EXEMPTION_FIELDS:
            value = item.get(field)
            if not isinstance(value, str) or not value.strip():
                raise SetupError("例外清单 %s 第 %d 条缺少必填字段 %s（需为非空字符串）" % (path, index, field))
            values[field] = value.strip()
        pattern = values["path"]
        if pattern.startswith("/") or "\\" in pattern or re.match(r"^[A-Za-z]:", pattern):
            raise SetupError("例外清单 %s 第 %d 条 path 必须为相对 main/ 的 posix 路径: %r" % (path, index, pattern))
        entries.append(ExemptionEntry(pattern=pattern, reason=values["reason"], date=values["date"]))
    return entries


def load_exemption_entries(script_dir: Path) -> List[ExemptionEntry]:
    """读取脚本同目录的例外清单；缺失视为无例外，解析失败抛 SetupError。"""
    path = script_dir / EXEMPTION_FILENAME
    if not path.is_file():
        print("[提示] 未找到例外清单 %s，视为无例外。" % path)
        return []
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise SetupError("无法读取例外清单 %s: %s" % (path, exc))
    try:
        import tomllib
    except ImportError:
        tomllib = None
    if tomllib is not None:
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as exc:
            raise SetupError("例外清单解析失败（%s）: %s" % (path, exc))
    else:
        print("[提示] 当前 Python 无内置 tomllib（Python 3.11+ 提供），已回退到受限 TOML 子集解析器。")
        try:
            data = parse_restricted_toml(text)
        except SetupError as exc:
            raise SetupError("例外清单解析失败（%s）: %s" % (path, exc))
    return extract_entries(data, path)


# ===== 结果归类 =====


def build_exempt_map(files: List[SourceFile], entries: List[ExemptionEntry]) -> Dict[str, ExemptionEntry]:
    """返回 rel_path -> 首个命中例外条目 的映射。"""
    mapping = {}
    for entry in entries:
        for item in files:
            if item.rel_path in mapping:
                continue
            if fnmatch.fnmatchcase(item.rel_path, entry.pattern):
                mapping[item.rel_path] = entry
    return mapping


def collect_stale_entries(
    files: List[SourceFile], entries: List[ExemptionEntry]
) -> List[Tuple[ExemptionEntry, str]]:
    """找出过期例外：无匹配文件，或匹配文件均已不超标。"""
    stale = []
    for entry in entries:
        matched = [item for item in files if fnmatch.fnmatchcase(item.rel_path, entry.pattern)]
        if not matched:
            stale.append((entry, "未找到匹配文件（文件不存在）"))
        elif not any(item.lines > LIMITS[item.category] for item in matched):
            stale.append((entry, "匹配文件均已不超标"))
    return stale


def partition(
    files: List[SourceFile], exempt_map: Dict[str, ExemptionEntry]
) -> Tuple[List[SourceFile], List[Tuple[SourceFile, ExemptionEntry]], List[Tuple[int, str]]]:
    """把文件分为阻断 / 例外命中 / 建议三组；阻断按行数降序、建议按数值降序。"""
    blocking = []
    exempt_hits = []
    suggestions = []
    for item in files:
        entry = exempt_map.get(item.rel_path)
        if entry is not None:
            if item.lines > LIMITS[item.category]:
                exempt_hits.append((item, entry))
            continue
        if item.lines > LIMITS[item.category]:
            blocking.append(item)
        elif item.category == "production" and item.lines > PRODUCTION_SUGGEST_LIMIT:
            suggestions.append(
                (
                    item.lines,
                    "%s: %d 行 > 建议线 %d（production）"
                    % (item.rel_path, item.lines, PRODUCTION_SUGGEST_LIMIT),
                )
            )
        if item.category == "production" and item.inline_test_lines is not None and item.inline_test_lines > INLINE_TEST_SUGGEST_LIMIT:
            suggestions.append(
                (
                    item.inline_test_lines,
                    "%s: 内联测试段 %d 行 > %d（production）"
                    % (item.rel_path, item.inline_test_lines, INLINE_TEST_SUGGEST_LIMIT),
                )
            )
    blocking.sort(key=lambda item: (-item.lines, item.rel_path))
    exempt_hits.sort(key=lambda pair: pair[0].rel_path)
    suggestions.sort(key=lambda pair: (-pair[0], pair[1]))
    return blocking, exempt_hits, suggestions


# ===== 报告输出 =====


def print_report(
    root: Path,
    files: List[SourceFile],
    stale: List[Tuple[ExemptionEntry, str]],
    blocking: List[SourceFile],
    exempt_hits: List[Tuple[SourceFile, ExemptionEntry]],
    suggestions: List[Tuple[int, str]],
) -> None:
    """按固定分节输出扫描概览、阻断清单、建议清单、例外与结论摘要。"""
    counts = {"barrel": 0, "test": 0, "production": 0}
    for item in files:
        counts[item.category] += 1
    print("Ramaria 结构红线自检")
    print("root: %s" % root)
    print(
        "扫描 .rs 文件: %d 个（barrel=%d / test=%d / production=%d）"
        % (len(files), counts["barrel"], counts["test"], counts["production"])
    )

    print()
    print("阻断清单（%d 项，按行数降序）:" % len(blocking))
    if blocking:
        for item in blocking:
            print(
                "  %s: %d 行 > %d 行（%s）"
                % (item.rel_path, item.lines, LIMITS[item.category], item.category)
            )
    else:
        print("  （无）")

    print()
    print("建议清单（%d 项）:" % len(suggestions))
    if suggestions:
        for _, text in suggestions:
            print("  %s" % text)
    else:
        print("  （无）")

    print()
    print("例外（登记，%d 项）:" % len(exempt_hits))
    if exempt_hits:
        for item, entry in exempt_hits:
            print(
                "  %s: %d 行 > %d 行（%s）；登记日期 %s；理由：%s"
                % (item.rel_path, item.lines, LIMITS[item.category], item.category, entry.date, entry.reason)
            )
    else:
        print("  （无）")

    print()
    print("过期例外（警告，%d 项）:" % len(stale))
    if stale:
        for entry, why in stale:
            print("  %s: %s（登记日期 %s）" % (entry.pattern, why, entry.date))
    else:
        print("  （无）")

    print()
    print(
        "结论: 阻断 %d 项 / 建议 %d 项 / 例外 %d 项 / 过期例外 %d 项 → %s"
        % (
            len(blocking),
            len(suggestions),
            len(exempt_hits),
            len(stale),
            "通过" if not blocking else "不通过",
        )
    )


# ===== 入口 =====


def main(argv: Optional[List[str]] = None) -> int:
    """解析参数、执行扫描并按结果返回退出码。"""
    parser = argparse.ArgumentParser(
        description="Ramaria 结构红线自检：扫描 crates 下 Rust 文件行数并对照结构阈值。"
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
        entries = load_exemption_entries(script_dir)
        files = scan_crates(root)
    except SetupError as exc:
        print("[错误] %s" % exc, file=sys.stderr)
        return 2

    exempt_map = build_exempt_map(files, entries)
    stale = collect_stale_entries(files, entries)
    blocking, exempt_hits, suggestions = partition(files, exempt_map)
    print_report(root, files, stale, blocking, exempt_hits, suggestions)
    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main())
