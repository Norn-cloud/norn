#!/usr/bin/env bash
# Shared precondition for published drops; candidate rehearsals use their commands directly.
drop_main() {
  if [[ $# != 0 ]]; then printf 'usage: bash %s\n' "$0" >&2; return 2; fi
  cd "$(dirname "${BASH_SOURCE[0]}")/../.."
  local main source state
  source=$(git rev-parse HEAD)
  if ! main=$(git rev-parse --verify refs/remotes/origin/main 2>/dev/null); then
    printf 'not yet: fetch origin/main before rehearsing a drop\n'; return 1
  fi
  if ! git merge-base --is-ancestor "$source" "$main"; then
    printf 'not yet: source %s is not on fetched origin/main\n' "$source"; return 1
  fi
  if ! state=$(git status --porcelain --untracked-files=normal); then
    printf 'not yet: git status could not establish a clean checkout\n'; return 1
  fi
  if [[ -n $state ]]; then
    printf 'not yet: a drop needs a clean main checkout\n'; return 1
  fi
  printf 'Main source: %s\n' "$source"
}
