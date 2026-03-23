#!/usr/bin/env bash
# dump_src.sh — print every Rust source file for pasting into Claude
set -euo pipefail

find src -name "*.rs" | sort | while read -r f; do
    echo "==== $f ===="
    cat "$f"
    echo ""
done
