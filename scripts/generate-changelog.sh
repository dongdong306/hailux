#!/usr/bin/env bash
# Generates a formatted changelog from git commits between two refs.
# Usage: ./scripts/generate-changelog.sh <from-tag> <to-ref>
# Example: ./scripts/generate-changelog.sh v0.1.0 v0.1.1
#          ./scripts/generate-changelog.sh v0.1.0 HEAD

set -euo pipefail

FROM="${1:?Missing <from> argument}"
TO="${2:-HEAD}"

SECTIONS_IMPROVEMENTS=()
SECTIONS_BUGFIXES=()
SECTIONS_CI=()
SECTIONS_REFACTOR=()
SECTIONS_DOCS=()
SECTIONS_PERF=()

# Parse conventional commit messages and categorize
while IFS= read -r line; do
    msg="$line"

    # Skip merge commits and revert commits
    [[ "$msg" =~ ^Merge ]] && continue
    [[ "$msg" =~ ^Revert ]] && continue

    # Determine type from conventional commit prefix
    type=""
    if [[ "$msg" =~ ^(feat|feature)(\(.+\))?(!)?: ]]; then
        type="feat"
    elif [[ "$msg" =~ ^fix(\(.+\))?(!)?: ]]; then
        type="fix"
    elif [[ "$msg" =~ ^ci(\(.+\))?(!)?: ]]; then
        type="ci"
    elif [[ "$msg" =~ ^refactor(\(.+\))?(!)?: ]]; then
        type="refactor"
    elif [[ "$msg" =~ ^perf(\(.+\))?(!)?: ]]; then
        type="perf"
    elif [[ "$msg" =~ ^docs(\(.+\))?(!)?: ]]; then
        type="docs"
    elif [[ "$msg" =~ ^(chore|test|style|build)(\(.+\))?(!)?: ]]; then
        continue
    else
        continue
    fi

    # Extract description: remove the type prefix
    desc=$(echo "$msg" | sed -E 's/^[a-z]+(\(.+\))?!?:\s*//')
    entry="- ${desc}"

    case "$type" in
        feat)     SECTIONS_IMPROVEMENTS+=("$entry") ;;
        fix)      SECTIONS_BUGFIXES+=("$entry") ;;
        ci)       SECTIONS_CI+=("$entry") ;;
        refactor) SECTIONS_REFACTOR+=("$entry") ;;
        perf)     SECTIONS_PERF+=("$entry") ;;
        docs)     SECTIONS_DOCS+=("$entry") ;;
    esac
done < <(git log "${FROM}..${TO}" --format='%s' --no-merges)

# Generate output
print_section() {
    local title="$1"
    shift
    local entries=("$@")
    if [ ${#entries[@]} -gt 0 ]; then
        echo "### ${title}"
        printf '%s\n' "${entries[@]}"
        echo ""
    fi
}

if [ ${#SECTIONS_IMPROVEMENTS[@]} -eq 0 ] && [ ${#SECTIONS_BUGFIXES[@]} -eq 0 ] && [ ${#SECTIONS_CI[@]} -eq 0 ] && [ ${#SECTIONS_REFACTOR[@]} -eq 0 ] && [ ${#SECTIONS_PERF[@]} -eq 0 ] && [ ${#SECTIONS_DOCS[@]} -eq 0 ]; then
    echo "No notable changes."
    exit 0
fi

print_section "Improvements" "${SECTIONS_IMPROVEMENTS[@]}"
print_section "Bugfixes" "${SECTIONS_BUGFIXES[@]}"
print_section "Performance" "${SECTIONS_PERF[@]}"
print_section "Refactor" "${SECTIONS_REFACTOR[@]}"
print_section "CI" "${SECTIONS_CI[@]}"
print_section "Documentation" "${SECTIONS_DOCS[@]}"

echo "---"
echo ""
echo "**Full Changelog**: https://github.com/dongdong306/hailux/compare/${FROM}...${TO}"
