#!/usr/bin/env bash
# Move commits must leave shipped resources, test baselines and CI alone.
set -euo pipefail
if (( $# < 1 || $# > 2 )); then
    echo "Usage: $0 <base> [head]" >&2
    exit 2
fi

base=$1
head=${2:-HEAD}
commits=$(git rev-list --no-merges "$base..$head")
for commit in $commits; do
    subject=$(git show -s --format=%s "$commit")
    [[ $subject == move:* ]] || continue
    changed=$(git diff --name-only "$commit^" "$commit" -- \
        Languages tools/snapshots assets help extension installer .github Cargo.toml Cargo.lock)
    if [[ -n $changed ]]; then
        printf '%s touches protected paths:\n%s\n' "$subject" "$changed" >&2
        exit 1
    fi
done
git diff --check "$base" "$head"
