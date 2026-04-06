#!/bin/sh
# kew status line for Claude Code — injected by `kew init`
# Reads workspace context from stdin, queries kew, renders a compact status bar.

input=$(cat)

project_dir=$(printf '%s' "$input" | jq -r '.workspace.project_dir // .cwd // empty' 2>/dev/null)
db_path=""
if [ -n "$project_dir" ] && [ -f "$project_dir/.kew/kew.db" ]; then
  db_path="$project_dir/.kew/kew.db"
else
  db_path="$HOME/.local/share/kew/kew.db"
fi

kew_status=""
if command -v kew >/dev/null 2>&1 && [ -f "$db_path" ]; then
  kew_status=$(kew --db "$db_path" status --porcelain 2>/dev/null)
fi

if [ -z "$kew_status" ]; then
  printf "◆ kew  offline"
  exit 0
fi

_get() { printf '%s' "$kew_status" | grep -o "$1=[^ ]*" | cut -d= -f2; }

running=$(_get running)
pending=$(_get pending)
done_count=$(_get done)
failed=$(_get failed)
context=$(_get context)
embeddings=$(_get embeddings)
db_size=$(_get db)
agents=$(_get agents)

parts=""
if [ "${running:-0}" -gt 0 ]; then
  parts="${parts}▶${running} "
fi
if [ "${pending:-0}" -gt 0 ]; then
  parts="${parts}⏳${pending} "
fi
if [ "${done_count:-0}" -gt 0 ]; then
  parts="${parts}✓${done_count} "
fi
if [ "${failed:-0}" -gt 0 ]; then
  parts="${parts}✗${failed} "
fi
if [ -z "$parts" ]; then
  parts="idle "
fi
parts="${parts}  ctx:${context:-0} emb:${embeddings:-0}"
parts="${parts}  db:${db_size:-?}"
if [ -n "$agents" ]; then
  parts="${parts}  [${agents}]"
fi

printf "◆ kew  %s" "$parts"
