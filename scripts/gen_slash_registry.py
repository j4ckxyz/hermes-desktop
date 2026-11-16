#!/usr/bin/env python3
"""Regenerate crates/core/src/slash_registry.json from an official hermes-agent checkout.

Usage: python3 -I scripts/gen_slash_registry.py /path/to/hermes-agent > crates/core/src/slash_registry.json

Reads `hermes_cli/commands.py` statically (ast, no import) and keeps every command a gateway
serves (not `cli_only`, or `cli_only` with a `gateway_config_gate`): the set Discord, Telegram and
the other messaging adapters expose. The app prefers the live `commands.catalog` RPC; this copy is
the offline fallback, the same pattern as the official desktop's `desktop-slash-registry.json`.
"""
import ast
import json
import sys
from pathlib import Path

src = Path(sys.argv[1]) / "hermes_cli" / "commands.py"
tree = ast.parse(src.read_text())
def lit(n):
    """A literal value; an f-string keeps only its constant parts."""
    try:
        return ast.literal_eval(n)
    except ValueError:
        if isinstance(n, ast.JoinedStr):
            return "".join(v.value for v in n.values if isinstance(v, ast.Constant)).strip()
        return None


out = []
for node in ast.walk(tree):
    if not (isinstance(node, ast.AnnAssign) and getattr(node.target, "id", "") == "COMMAND_REGISTRY"):
        continue
    for call in node.value.elts:
        if not (isinstance(call, ast.Call) and getattr(call.func, "id", "") == "CommandDef"):
            continue
        args = [lit(a) for a in call.args]
        kw = {k.arg: lit(k.value) for k in call.keywords}
        name, desc, cat = args[0], args[1], args[2]
        if kw.get("cli_only") and not kw.get("gateway_config_gate"):
            continue
        out.append({
            "name": name,
            "description": desc,
            "category": cat,
            "aliases": list(kw.get("aliases") or ()),
            "args": kw.get("args_hint") or "",
            "subcommands": list(kw.get("subcommands") or ()),
            "gateway_only": bool(kw.get("gateway_only", False)),
            "desktop": kw.get("desktop"),
        })
json.dump(out, sys.stdout, indent=1, ensure_ascii=False)
print()
