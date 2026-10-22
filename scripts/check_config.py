#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 配置键面校验脚本。

职责:
- 扫描 config/default.toml 的 section 与键（不依赖第三方 TOML 库）；
- 与 scripts/config-baseline-3.0.json（3.0 键面机器基线）比对：section 集合、键集合、重复键；
- 交叉校验每个键名在 ramaria-core/src/config/**/*.rs 中存在同名 pub 字段（模板与结构体不脱节）；
- 不一致即退出码 1。

用法:
- python scripts/check_config.py
- python scripts/check_config.py --update   （配置键面经审定的变更后重新生成基线）

退出码: 0 一致；1 不一致；2 用法 / 环境错误。
"""

import argparse
import io
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Set, Tuple

CONFIG_REL = Path("config/default.toml")
BASELINE_REL = Path("scripts/config-baseline-3.0.json")
CONFIG_RS_DIR = Path("crates/ramaria-core/src/config")


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


def scan_config(path: Path) -> Tuple[List[str], List[str]]:
    try:
        lines = io.open(path, encoding="utf-8").read().splitlines()
    except OSError as exc:
        raise SetupError("无法读取配置模板 %s: %s" % (path, exc))

    sections: List[str] = []
    keys: List[str] = []
    section = ""
    for number, line in enumerate(lines, 1):
        text = line.strip()
        if not text or text.startswith("#"):
            continue
        header = re.match(r"^\[([^\]]+)\]$", text)
        if header:
            section = header.group(1)
            if section not in sections:
                sections.append(section)
            continue
        if text.startswith("[["):
            raise SetupError("第 %d 行：不支持数组表 [[...]]" % number)
        entry = re.match(r"^([a-z_][a-z0-9_]*)\s*=", text)
        if entry:
            keys.append("%s.%s" % (section, entry.group(1)) if section else entry.group(1))
    return sections, keys


def collect_rust_fields(root: Path) -> Set[str]:
    fields: Set[str] = set()
    for path in sorted((root / CONFIG_RS_DIR).rglob("*.rs")):
        text = io.open(path, encoding="utf-8").read()
        for match in re.finditer(r"^\s*pub ([a-z_][a-z0-9_]*):", text, re.M):
            fields.add(match.group(1))
    if not fields:
        raise SetupError("未在 %s 下解析到任何 pub 字段" % CONFIG_RS_DIR)
    return fields


def load_baseline(path: Path) -> Dict:
    try:
        return json.loads(io.open(path, encoding="utf-8").read())
    except OSError as exc:
        raise SetupError("无法读取配置基线 %s: %s" % (path, exc))
    except ValueError as exc:
        raise SetupError("配置基线 JSON 解析失败: %s" % exc)


def main(argv: List[str]) -> int:
    reconfigure_streams()
    parser = argparse.ArgumentParser(description="校验 Ramaria 3.0 配置键面")
    parser.add_argument("--update", action="store_true", help="重新生成键面基线")
    parser.add_argument("--root", default=None, help="main 仓库根目录（默认脚本上一级）")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    config_path = root / CONFIG_REL
    baseline_path = root / BASELINE_REL

    try:
        sections, keys = scan_config(config_path)
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    duplicates = sorted({key for key in keys if keys.count(key) > 1})
    if duplicates:
        print("[FAIL] 配置模板存在重复键: %s" % ", ".join(duplicates))
        return 1

    if args.update:
        payload = {
            "source": "config/default.toml（3.0 键面机器基线；键面变更经审定后更新）",
            "sections": sections,
            "keys": keys,
        }
        baseline_path.parent.mkdir(parents=True, exist_ok=True)
        io.open(baseline_path, "w", encoding="utf-8", newline="\n").write(
            json.dumps(payload, ensure_ascii=False, indent=2) + "\n"
        )
        print("[OK] 已生成配置键面基线: %s（%d 组 / %d 键）" % (BASELINE_REL.as_posix(), len(sections), len(keys)))
        return 0

    try:
        baseline = load_baseline(baseline_path)
        rust_fields = collect_rust_fields(root)
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    failures: List[str] = []
    expected_sections = baseline.get("sections", [])
    expected_keys = baseline.get("keys", [])

    actual_section_set = set(sections)
    expected_section_set = set(expected_sections)
    if actual_section_set != expected_section_set:
        missing = sorted(expected_section_set - actual_section_set)
        extra = sorted(actual_section_set - expected_section_set)
        failures.append(
            "配置组: 实际 %d / 期望 %d%s%s"
            % (
                len(sections),
                len(expected_sections),
                "；缺少: " + ", ".join(missing) if missing else "",
                "；多出: " + ", ".join(extra) if extra else "",
            )
        )

    actual_key_set = set(keys)
    expected_key_set = set(expected_keys)
    if actual_key_set != expected_key_set:
        missing = sorted(expected_key_set - actual_key_set)
        extra = sorted(actual_key_set - expected_key_set)
        failures.append(
            "配置键: 实际 %d / 期望 %d%s%s"
            % (
                len(keys),
                len(expected_keys),
                "；缺少: " + ", ".join(missing) if missing else "",
                "；多出: " + ", ".join(extra) if extra else "",
            )
        )

    unknown = sorted(
        {key for key in keys if key.split(".")[-1] not in rust_fields}
    )
    if unknown:
        failures.append("模板键在 ramaria-core 配置结构中无同名字段: %s" % ", ".join(unknown))

    if failures:
        print("[FAIL] 配置键面不一致:")
        for failure in failures:
            print("  - %s" % failure)
        return 1

    print(
        "[OK] 配置键面一致：%d 组 / %d 键；模板键均有同名 Rust 字段"
        % (len(sections), len(keys))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
