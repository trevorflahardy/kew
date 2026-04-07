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

BOLD='\033[1m'
RED='\033[31m'
GREEN='\033[32m'
YELLOW='\033[33m'
CYAN='\033[36m'
DIM='\033[2m'
RESET='\033[0m'

parts=""
if [ "${running:-0}" -gt 0 ]; then
  parts="${parts}${CYAN}${BOLD}▶${running}${RESET} "
fi
if [ "${pending:-0}" -gt 0 ]; then
  parts="${parts}${YELLOW}⏳${pending}${RESET} "
fi
if [ "${done_count:-0}" -gt 0 ]; then
  parts="${parts}${GREEN}✓${done_count}${RESET} "
fi
if [ "${failed:-0}" -gt 0 ]; then
  parts="${parts}${RED}${BOLD}✗${failed}${RESET} "
fi
if [ -z "$parts" ]; then
  parts="${DIM}idle${RESET} "
fi
parts="${parts} ${DIM}ctx:${context:-0} emb:${embeddings:-0} db:${db_size:-?}${RESET}"
if [ -n "$agents" ]; then
  parts="${parts} ${CYAN}[${agents}]${RESET}"
fi

printf "◆ kew  ${parts}"
