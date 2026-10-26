#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成中文字体子集（woff2）。

职责:
- 从完整字体文件裁出 GB2312 全量汉字 + ASCII 可见字符 + 界面补充符号，输出单文件 woff2；
- 供 `src/assets/fonts/` 的中文衬线资产重建使用。

用法:
    python scripts/subset-font.py --source <源字体.ttf> --output src/assets/fonts/<目标>.woff2

依赖:
    pip install fonttools brotli

退出码: 0 成功；2 用法或输入错误；其余为 pyftsubset 的失败码。
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

# 界面补充符号：GB2312 未收录但界面文案会用到的少量字符
EXTRA_CHARS = "→←↑↓·•…—–✓×÷±§¶†‡℃№★☆○●◇◆□■△▲▽▼※≈≠≤≥∞"


def gb2312_chars() -> set:
    """枚举 GB2312 双字节区全部可解码字符（6763 汉字 + 符号区）。"""
    chars = set()
    for high in range(0xA1, 0xF8):
        for low in range(0xA1, 0xFF):
            try:
                chars.add(bytes([high, low]).decode("gb2312"))
            except UnicodeDecodeError:
                continue
    return chars


def main(argv) -> int:
    parser = argparse.ArgumentParser(description="生成中文字体子集（woff2）")
    parser.add_argument("--source", required=True, help="源字体文件（ttf / otf）")
    parser.add_argument("--output", required=True, help="输出 woff2 路径")
    parser.add_argument("--keep-chars", default=None, help="额外保留字符（默认用内置界面补充集）")
    args = parser.parse_args(argv)

    source = Path(args.source)
    if not source.is_file():
        print("[ERROR] 源字体不存在: %s" % source, file=sys.stderr)
        return 2

    chars = gb2312_chars()
    chars.update(chr(code) for code in range(0x20, 0x7F))
    chars.update(args.keep_chars if args.keep_chars is not None else EXTRA_CHARS)

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as workdir:
        text_file = Path(workdir) / "chars.txt"
        text_file.write_text("".join(sorted(chars)), encoding="utf-8")
        command = [
            sys.executable,
            "-m",
            "fontTools.subset",
            str(source),
            "--text-file=%s" % text_file,
            "--flavor=woff2",
            "--layout-features=*",
            "--output-file=%s" % output,
        ]
        result = subprocess.run(command, check=False)
        if result.returncode != 0:
            print("[ERROR] pyftsubset 失败，退出码 %d" % result.returncode, file=sys.stderr)
            return result.returncode

    size_mb = output.stat().st_size / 1024 / 1024
    print("[OK] %s：字符 %d 个 / %.2f MB" % (output, len(chars), size_mb))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
