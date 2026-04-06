#!/bin/sh
# kew status line for Claude Code
# Reads Claude context from stdin, queries kew DB, formats a compact status bar.

input=$(cat)

# Determine the DB path: prefer .kew/kew.db in the project dir, else global
project_dir=$(printf '%s' "$input" | jq -r '.workspace.project_dir // .cwd // empty' 2>/dev/null)
db_path=""
if [ -n "$project_dir" ] && [ -f "$project_dir/.kew/kew.db" ]; then
  db_path="$project_dir/.kew/kew.db"
else
  db_path="$HOME/.local/share/kew/kew.db"
fi

# Query kew for machine-readable status (fail silently if not installed or no DB)
kew_status=""
if command -v kew >/dev/null 2>&1 && [ -f "$db_path" ]; then
  kew_status=$(kew --db "$db_path" status --porcelain 2>/dev/null)
fi

if [ -z "$kew_status" ]; then
  # kew not running or no DB yet — show minimal indicator
  printf "◆ kew  offline"
  exit 0
fi

# Parse key=value pairs
_get() { printf '%s' "$kew_status" | grep -o "$1=[^ ]*" | cut -d= -f2; }

running=$(_get running)
pending=$(_get pending)
done_count=$(_get done)
failed=$(_get failed)
context=$(_get context)
embeddings=$(_get embeddings)
db_size=$(_get db)

# Build the status segments
parts=""

# Agent activity
if [ "${running:-0}" -gt 0 ]; then
  parts="${parts}▶ ${running} "
else
  parts="${parts}▷ 0 "
fi

if [ "${pending:-0}" -gt 0 ]; then
  parts="${parts}⏳${pending} "
fi

parts="${parts}✓${done_count:-0}"

if [ "${failed:-0}" -gt 0 ]; then
  parts="${parts} ✗${failed}"
fi

# Knowledge base
parts="${parts}  ctx:${context:-0} emb:${embeddings:-0}"

# DB size (omit if zero/unknown)
if [ -n "$db_size" ] && [ "$db_size" != "0KB" ]; then
  parts="${parts} ${db_size}"
fi

printf "◆ kew  %s" "$parts"
