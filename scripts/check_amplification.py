#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 变更放大系数探针。

职责:
- 按 D-V30-012 的四项实验做静态计数：加领域字段 / 加存储方法 / 改提示词 / 改前端命令名；
- 输出当前计数与目标，供 M4-2 / M4-3 验收与 M8 复测引用；
- 只读，不修改任何文件。

用法:
- python scripts/check_amplification.py
- python scripts/check_amplification.py --root f:/Ramaria-s/main

退出码: 0 正常输出；2 用法 / 环境错误。
"""

import argparse
import os
import re
import sys
from pathlib import Path
from typing import List, Optional, Tuple

from check_test_debt import SetupError, read_text, reconfigure_streams, scan_rust

# ===== 口径与目标 =====

TARGET_MEMORY_EVENT = 2
TARGET_STORE_CRUD = 1
TARGET_GOLDEN = 1
TARGET_FRONTEND_INVOKE_OUTSIDE = 0
GOLDEN_REL = Path("crates/ramaria-service/tests/parity/golden")
FRONTEND_SRC_REL = Path("crates/ramaria-desktop/frontend/src")
FRONTEND_TRANSPORT_REL = "crates/ramaria-desktop/frontend/src/core/transport.ts"
FRONTEND_GATEWAY_REL = "crates/ramaria-desktop/frontend/src/core/api/index.ts"
FRONTEND_COMMANDS_REL = "crates/ramaria-desktop/frontend/src/core/api/commands.ts"


def _is_return_type(text: str, start: int) -> bool:
    """判断匹配点是否位于函数返回类型位置（箭头后 path::T 的花括号属于函数体）。"""
    window = text[max(0, start - 160) : start]
    boundary = max(window.rfind(";"), window.rfind("{"), window.rfind("}"))
    return "->" in window[boundary + 1 :]


def count_memory_event_files(root: Path) -> int:
    """构造 MemoryEvent 字面量（测试段内）的测试文件数。

    只统计表达式位置的结构体字面量：函数返回类型写作 `-> MemoryEvent {`
    时花括号属于函数体，不是字面量，需排除。
    """
    pattern = re.compile(r"\bMemoryEvent\s*\{")
    count = 0
    for item in scan_rust(root):
        if not item.test_text:
            continue
        for match in pattern.finditer(item.test_text):
            if _is_return_type(item.test_text, match.start()):
                continue
            count += 1
            break
    return count


def count_store_crud_impls(root: Path) -> Tuple[int, int]:
    """返回（全部 impl 文件数，测试侧 impl 文件数）。"""
    total = 0
    test_side = 0
    for dirpath, dirnames, filenames in os.walk(str(root / "crates")):
        dirnames[:] = sorted(name for name in dirnames if name != "target")
        for filename in sorted(filenames):
            if not filename.endswith(".rs"):
                continue
            abs_path = Path(dirpath) / filename
            if "impl StoreCrud for" not in read_text(abs_path):
                continue
            total += 1
            rel_path = os.path.relpath(str(abs_path), str(root)).replace(os.sep, "/")
            parts = rel_path.split("/")
            if "tests" in parts[:-1] or re.match(r".*tests.*\.rs$", filename) or "test_support" in parts:
                test_side += 1
    return total, test_side


def count_golden(root: Path) -> Tuple[int, int]:
    """返回（提示词敏感快照数，快照总数）。

    只有断言系统提示词形状或模板版本的快照会因改一句提示词而失效；
    其余快照锁的是检索、封存、索引等自身行为，不计入提示词放大系数。
    """
    golden_dir = root / GOLDEN_REL
    if not golden_dir.is_dir():
        return 0, 0
    total = 0
    sensitive = 0
    for path in sorted(golden_dir.glob("*.json")):
        total += 1
        text = read_text(path)
        if "system_prompt" in text or "template_version" in text:
            sensitive += 1
    return sensitive, total


def count_frontend_invoke(root: Path) -> Tuple[int, int]:
    """返回（命令门面之外直接调用 invoke 的文件数，封装表单点数）。"""
    src_dir = root / FRONTEND_SRC_REL
    if not src_dir.is_dir():
        return 0, 0
    commands = root / FRONTEND_COMMANDS_REL
    wrapper = 1 if commands.is_file() else 0
    outside = 0
    for abs_path in sorted(list(src_dir.rglob("*.ts")) + list(src_dir.rglob("*.vue"))):
        rel_path = os.path.relpath(str(abs_path), str(root)).replace(os.sep, "/")
        if rel_path in (FRONTEND_TRANSPORT_REL, FRONTEND_GATEWAY_REL):
            continue
        if re.search(r"\binvoke\s*[<(]", read_text(abs_path)):
            outside += 1
    return outside, wrapper


def render(root: Path) -> None:
    memory_event = count_memory_event_files(root)
    store_total, store_test = count_store_crud_impls(root)
    golden, golden_total = count_golden(root)
    invoke_outside, invoke_wrapper = count_frontend_invoke(root)

    print("== 变更放大系数 ==")
    rows: List[Tuple[str, str, str, bool]] = [
        (
            "加 MemoryEvent 字段",
            "%d 个测试文件" % memory_event,
            "≤%d" % TARGET_MEMORY_EVENT,
            memory_event <= TARGET_MEMORY_EVENT,
        ),
        (
            "加 StoreCrud 方法",
            "测试侧 %d（全部 %d）" % (store_test, store_total),
            "测试侧 ≤%d" % TARGET_STORE_CRUD,
            store_test <= TARGET_STORE_CRUD,
        ),
        (
            "改一句提示词",
            "%d 份 golden（共 %d 份）" % (golden, golden_total),
            "≤%d 份对应用例" % TARGET_GOLDEN,
            golden <= TARGET_GOLDEN,
        ),
        (
            "改前端命令名",
            "封装表 %d / 直调 %d 文件" % (invoke_wrapper, invoke_outside),
            "直调 ≤%d（命令表单点）" % TARGET_FRONTEND_INVOKE_OUTSIDE,
            invoke_outside <= TARGET_FRONTEND_INVOKE_OUTSIDE,
        ),
    ]
    for name, current, target, ok in rows:
        print("%-18s %-22s 目标 %-14s %s" % (name, current, target, "达成" if ok else "未达"))


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="Ramaria 变更放大系数探针：四项实验的静态计数与目标对照。"
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
