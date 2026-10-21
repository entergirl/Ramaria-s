#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Ramaria 数据库 schema 快照校验脚本。

职责:
- 把 ramaria-storage/migrations/*.sql 应用到内存 SQLite（迁移驱动，不手写 DDL）；
- 生成规范化 schema 快照（表 / 索引 DDL + 计数头），与 scripts/schema/expected.sql 比对（文档仓副本存在时同步校验）；
- 快照不一致、migration 不是单文件、schema_version 不是 3 时退出码 1。

用法:
- python scripts/check_schema.py            校验（CI / 本地）
- python scripts/check_schema.py --update   重新生成主快照并同步文档副本（schema 变更并已审定时）

退出码:
- 0: 一致；1: 不一致或基线不满足；2: 用法 / 环境错误。
"""

import argparse
import difflib
import io
import sqlite3
import sys
from pathlib import Path
from typing import Dict, List, Tuple

MIGRATION_GLOB = "*.sql"
CONTRACT_BASELINE = Path("scripts/contract-baseline-3.0.json")
EXPECTED_VERSION = "3"
EXPECTED_REL = Path("scripts/schema/expected.sql")
EXPECTED_DOC_REL = Path("../docs/dev-3.0/test/schema/expected.sql")


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


def load_migrations(migration_dir: Path) -> List[Path]:
    if not migration_dir.is_dir():
        raise SetupError("migration 目录不存在: %s" % migration_dir)
    files = sorted(migration_dir.glob(MIGRATION_GLOB))
    if not files:
        raise SetupError("migration 目录为空: %s" % migration_dir)
    return files


def build_database(files: List[Path]) -> sqlite3.Connection:
    conn = sqlite3.connect(":memory:")
    try:
        for path in files:
            sql = io.open(path, encoding="utf-8").read()
            conn.executescript(sql)
    except sqlite3.Error as exc:
        conn.close()
        raise SetupError("migration 执行失败: %s" % exc)
    return conn


def count_columns(conn: sqlite3.Connection, tables: List[str]) -> int:
    total = 0
    for table in tables:
        total += len(conn.execute('PRAGMA table_info("%s")' % table).fetchall())
    return total


def count_foreign_keys(conn: sqlite3.Connection, tables: List[str]) -> int:
    total = 0
    for table in tables:
        total += len(conn.execute('PRAGMA foreign_key_list("%s")' % table).fetchall())
    return total


def load_contract_counts(root: Path) -> Dict:
    import json

    try:
        return json.loads(io.open(root / CONTRACT_BASELINE, encoding="utf-8").read())["schema"]
    except (OSError, ValueError, KeyError) as exc:
        raise SetupError("无法读取契约基线 %s: %s" % (CONTRACT_BASELINE.as_posix(), exc))


def build_snapshot(conn: sqlite3.Connection, files: List[Path]) -> str:
    tables = [
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_master "
            "WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    indexes = [
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_master "
            "WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    version_row = conn.execute(
        "SELECT value FROM schema_meta WHERE key='schema_version'"
    ).fetchone()
    version = version_row[0] if version_row else "<missing>"

    lines = [
        "-- Ramaria schema 快照（自动生成，请勿手改）",
        "-- 生成命令: python scripts/check_schema.py --update",
        "-- migration: %s" % ", ".join(path.name for path in files),
        "-- 统计: 表 %d / 列 %d / 显式索引 %d / 外键 %d / schema_version=%s"
        % (
            len(tables),
            count_columns(conn, tables),
            len(indexes),
            count_foreign_keys(conn, tables),
            version,
        ),
        "",
    ]

    for table in tables:
        row = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?", (table,)
        ).fetchone()
        lines.append("-- table: %s" % table)
        lines.append((row[0] or "").strip())
        lines.append("")

    for index in indexes:
        row = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name=?", (index,)
        ).fetchone()
        sql = (row[0] or "").strip()
        if not sql:
            continue
        lines.append("-- index: %s" % index)
        lines.append(sql)
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def read_expected(path: Path) -> str:
    try:
        return io.open(path, encoding="utf-8").read()
    except OSError as exc:
        raise SetupError("无法读取快照 %s: %s" % (path, exc))


def main(argv: List[str]) -> int:
    reconfigure_streams()
    parser = argparse.ArgumentParser(description="校验 Ramaria 数据库 schema 快照")
    parser.add_argument("--update", action="store_true", help="重新生成快照")
    parser.add_argument("--root", default=None, help="仓库根目录（默认脚本上一级）")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    migration_dir = root / "crates/ramaria-storage/migrations"
    expected_path = root / EXPECTED_REL
    doc_path = root / EXPECTED_DOC_REL

    try:
        files = load_migrations(migration_dir)
        if len(files) != 1:
            print(
                "[FAIL] 3.0 基线要求单 migration 文件，实际 %d 个: %s"
                % (len(files), ", ".join(path.name for path in files))
            )
            return 1
        contract = load_contract_counts(root)
        conn = build_database(files)
        version = conn.execute(
            "SELECT value FROM schema_meta WHERE key='schema_version'"
        ).fetchone()
        if not version or version[0] != EXPECTED_VERSION:
            print("[FAIL] schema_version 应为 %s，实际 %s" % (EXPECTED_VERSION, version))
            conn.close()
            return 1
        tables = [
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
        ]
        indexes = [
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master "
                "WHERE type='index' AND name NOT LIKE 'sqlite_%'"
            )
        ]
        actual_counts = {
            "tables": len(tables),
            "columns": count_columns(conn, tables),
            "explicit_indexes": len(indexes),
            "foreign_keys": count_foreign_keys(conn, tables),
            "schema_version": version[0],
        }
        drift = {
            key: (actual_counts[key], contract.get(key))
            for key in actual_counts
            if contract.get(key) != actual_counts[key]
        }
        if drift:
            print("[FAIL] 数据面计数偏离契约基线: %s" % drift)
            conn.close()
            return 1
        snapshot = build_snapshot(conn, files)
        conn.close()
    except SetupError as exc:
        print("[ERROR] %s" % exc, file=sys.stderr)
        return 2

    if args.update:
        expected_path.parent.mkdir(parents=True, exist_ok=True)
        io.open(expected_path, "w", encoding="utf-8", newline="\n").write(snapshot)
        print("[OK] 已生成 schema 快照: %s" % expected_path.relative_to(root).as_posix())
        # 文档仓可见副本（CI 只检出 main，故以 scripts/schema 为主副本）
        if doc_path.parent.is_dir():
            io.open(doc_path, "w", encoding="utf-8", newline="\n").write(snapshot)
            print("[OK] 已同步文档副本: %s" % EXPECTED_DOC_REL.as_posix())
        return 0

    try:
        expected = read_expected(expected_path)
    except SetupError as exc:
        print("[ERROR] %s（首次请用 --update 生成）" % exc, file=sys.stderr)
        return 2

    if expected.replace("\r\n", "\n") == snapshot:
        if doc_path.is_file():
            doc_text = io.open(doc_path, encoding="utf-8").read().replace("\r\n", "\n")
            if doc_text != snapshot:
                print(
                    "[FAIL] 文档副本与主副本不一致: %s（请执行 --update 同步）"
                    % EXPECTED_DOC_REL.as_posix()
                )
                return 1
        print("[OK] schema 快照一致（%s）" % files[0].name)
        return 0

    diff = list(
        difflib.unified_diff(
            expected.replace("\r\n", "\n").splitlines(),
            snapshot.splitlines(),
            fromfile="expected.sql",
            tofile="migrations",
            lineterm="",
        )
    )
    print("[FAIL] schema 快照与 migration 不一致:")
    for line in diff[:80]:
        print(line)
    if len(diff) > 80:
        print("... （省略 %d 行）" % (len(diff) - 80))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
