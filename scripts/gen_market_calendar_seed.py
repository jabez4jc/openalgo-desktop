"""Regenerate the market holiday seed from OpenAlgo web.

Reads the holiday lists in database/market_calendar_db.py of a web checkout
(seed_holidays_<year> functions) without importing it, and writes them in
the same shape to src-tauri/src/db/sqlite/market_calendar_seed.json, which
the desktop's idempotent market calendar migration seeds from.

Usage (from the repository root):
    python3 scripts/gen_market_calendar_seed.py ../openalgo
"""

import ast
import json
import os
import sys


def main():
    web_root = sys.argv[1] if len(sys.argv) > 1 else "../openalgo"
    path = os.path.join(web_root, "database", "market_calendar_db.py")
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read())
    seed = {}
    for node in tree.body:
        if not (isinstance(node, ast.FunctionDef) and node.name.startswith("seed_holidays_")):
            continue
        year = node.name.rsplit("_", 1)[1]
        for stmt in node.body:
            if isinstance(stmt, ast.Assign) and isinstance(stmt.value, ast.List):
                seed[year] = ast.literal_eval(stmt.value)
    for year, rows in seed.items():
        for row in rows:
            row.setdefault("holiday_type", "TRADING_HOLIDAY")
    target = os.path.join("src-tauri", "src", "db", "sqlite", "market_calendar_seed.json")
    with open(target, "w", encoding="utf-8") as f:
        json.dump(seed, f, indent=1)
        f.write("\n")
    print(target, {y: len(r) for y, r in seed.items()})


if __name__ == "__main__":
    main()
