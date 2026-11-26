#!/usr/bin/env bash
# supply-chain-guard: read-only scan of a git tree for injected loaders.
#
#   scan.sh [REPO] [REF] [--range A..B] [--since DAYS] [--github OWNER/REPO]
#   scan.sh [REPO] --staged
#
# REPO is a working repository or a bare one (default: .). REF is any
# tree-ish (default: HEAD); --staged scans the index instead. Only git
# objects are read (git grep, git ls-tree, git log): nothing from the
# repository is checked out, installed, built, linted or run.
#
#   --range A..B        also scan the tree of every commit in A..B for markers
#   --since DAYS        list commits on REF from the last DAYS days whose
#                       committer date is over a day after the author date
#   --github OWNER/REPO list the repository's recent force-pushes (needs gh)
#
# Exit status: 2 when a HIT is found, 1 when only REVIEW items are, 0 clean.
# The patterns below are written so this file never matches itself.
set -u

REPO=.
REF=HEAD
STAGED=
RANGE=
SINCE=
GITHUB=
positional=0
while [ $# -gt 0 ]; do
    case "$1" in
        --staged) STAGED=1 ;;
        --range) RANGE=$2; shift ;;
        --since) SINCE=$2; shift ;;
        --github) GITHUB=$2; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *)
            if [ $positional -eq 0 ]; then REPO=$1; else REF=$1; fi
            positional=$((positional + 1)) ;;
    esac
    shift
done

g() { git -C "$REPO" "$@"; }
g rev-parse --git-dir > /dev/null 2>&1 || { echo "not a git repository: $REPO" >&2; exit 3; }
# git grep takes --cached before the pattern and a tree-ish after it.
if [ -n "$STAGED" ]; then
    CACHED=(--cached)
    SRC=()
    LABEL="index of $REPO"
    files() { g ls-files; }
else
    g rev-parse --verify --quiet "$REF^{tree}" > /dev/null || { echo "no such tree: $REF" >&2; exit 3; }
    CACHED=()
    SRC=("$REF")
    LABEL="$REPO at $REF ($(g rev-parse --short "$REF"))"
    files() { g ls-tree -r --name-only "$REF"; }
fi

HITS=0
REVIEWS=0
# Each finding: level, check name, then the matching lines (shortened).
emit() {
    local level=$1 check=$2 lines=$3
    [ -z "$lines" ] && return
    local count
    count=$(printf '%s\n' "$lines" | wc -l | tr -d ' ')
    echo "$level  $check ($count)"
    printf '%s\n' "$lines" | head -20 | sed -E 's/[[:blank:]]{150,}/ <<BLANKS>> /' |
        cut -c1-220 | sed 's/^/      /'
    if [ "$level" = HIT ]; then HITS=$((HITS + count)); else REVIEWS=$((REVIEWS + count)); fi
}
ggrep() { g grep -I -n "${CACHED[@]}" "$@"; }

echo "supply-chain-guard scan: $LABEL"

# 1. Loader markers: PolinRider's globals and _$_ tables, its seed string,
#    the Ethereum dead-drop address and C2 paths.
MARKERS='global\.o *= *['"'"'"]|global\[['"'"'"]!['"'"'"]\] *=|global\.i *= *['"'"'"]A8-|_\$_[0-9a-f]{4}|rmcej%ot[b]%|0xa322e5f3d311d3080e6f0121063e9adc2490ef1[a]|/0x/c?l[s]\b'
emit HIT "loader marker" "$(ggrep -i -E "$MARKERS" "${SRC[@]}" 2>/dev/null)"

# 2. Code hidden to the right: a long run of blanks with code after it.
emit HIT "code after a long run of blanks" \
    "$(ggrep -E '[[:blank:]]{150,}[^[:space:]]' "${SRC[@]}" 2>/dev/null)"

# 3. Invisible characters. GlassWorm encodes its payload as a long run of
#    variation selectors or private-use characters; emoji use single
#    selectors, so only runs of four or more count. Zero-width spaces and
#    joiners are left out: translations and Persian text are full of them.
#    Tag characters can carry text an AI agent reads and a person cannot
#    see. Direction controls (Trojan Source) can make code read differently
#    from how it runs.
INVISIBLE_RUN='[\x{FE00}-\x{FE0F}\x{E000}-\x{F8FF}\x{E0100}-\x{E01EF}]{4,}'
emit HIT "run of invisible characters" \
    "$(LC_ALL=C.UTF-8 ggrep -P "$INVISIBLE_RUN" "${SRC[@]}" 2>/dev/null | head -50)"
emit REVIEW "run of Unicode tag characters" \
    "$(LC_ALL=C.UTF-8 ggrep -P '[\x{E0000}-\x{E007F}]{4,}' "${SRC[@]}" 2>/dev/null | head -50)"
emit REVIEW "text direction control character" \
    "$(LC_ALL=C.UTF-8 ggrep -P '[\x{202A}-\x{202E}\x{2066}-\x{2069}]' "${SRC[@]}" 2>/dev/null | head -50)"

# 4. Tasks an editor runs on opening the folder.
emit HIT "editor task that runs on folder open" \
    "$(ggrep -E '"runOn"[[:space:]]*:[[:space:]]*"folderOpen"' "${SRC[@]}" -- '*.json' 2>/dev/null)"

# 5. Fonts and images that git reads as text: disguised scripts.
emit HIT "text file named as a font or image" \
    "$(g grep -I -l "${CACHED[@]}" -e . "${SRC[@]}" -- '*.woff' '*.woff2' '*.ttf' '*.otf' '*.eot' \
        '*.png' '*.jpg' '*.jpeg' '*.gif' '*.ico' '*.bmp' '*.webp' 2>/dev/null)"

# 6. Files named after known worm payloads.
emit HIT "known payload file name" \
    "$(files | grep -i -E '(^|/)(setup_bun|bun_environment)\.js$|shai-hulu[d]|sha1-hulu[d]')"

# 7. Install-time hooks: npm lifecycle scripts and composer scripts run on
#    install; review what they call.
emit REVIEW "install-time script" \
    "$(ggrep -E '"(preinstall|install|postinstall|prepare|prepublish|prepack|postpack)"[[:space:]]*:' \
        "${SRC[@]}" -- '*package.json' 2>/dev/null
       ggrep -E '"(pre|post)-(install|update)-cmd"[[:space:]]*:' "${SRC[@]}" -- '*composer.json' 2>/dev/null)"

# 8. Config files that tools load (and so run) doing what a config never
#    needs: eval, building functions from strings, spawning processes,
#    decoding blobs.
CONFIGS=('*.config.js' '*.config.mjs' '*.config.cjs' '*.config.ts' '*.config.mts' '*.config.cts'
         '*.eslintrc*' '*babel.config*' '*.babelrc*' '*.prettierrc*' '*.stylelintrc*' '*functions.php')
emit REVIEW "config file that evaluates or spawns" \
    "$(ggrep -E 'eval[[:space:]]*\(|Function[[:space:]]*\(|createRequire|child_process|\.unref\(\)|atob\(|fromCharCode|base64' \
        "${SRC[@]}" -- "${CONFIGS[@]}" 2>/dev/null)"

# 9. .gitignore lines that hide scripts or executables from review.
emit REVIEW "ignored script or executable" \
    "$(ggrep -E '\.(bat|cmd|ps1|vbs|exe|dll|scr)[[:space:]]*$' "${SRC[@]}" -- '*.gitignore' 2>/dev/null)"

# 10. Every commit in a range, for markers.
if [ -n "$RANGE" ]; then
    found=
    for commit in $(g rev-list "$RANGE"); do
        hit=$(g grep -I -l -i -E "$MARKERS" "$commit" 2>/dev/null | head -3 | tr '\n' ' ')
        [ -n "$hit" ] && found="$found$(g log -1 --format='%h %cI %cn' "$commit") $hit"$'\n'
    done
    emit HIT "loader marker in a commit of $RANGE" "${found%$'\n'}"
    echo "      ($(g rev-list --count "$RANGE") commits of $RANGE scanned)"
fi

# 11. Commits rewritten long after they were written: a forced rewrite keeps
#     the author date and gets a new committer date.
if [ -n "$SINCE" ] && [ -z "$STAGED" ]; then
    emit REVIEW "committer date over a day after the author date (last $SINCE days)" \
        "$(g log "$REF" --since="$SINCE days ago" --format='%h %at %ct %ad | %cd %cn | %s' --date=iso |
            awk '$3 - $2 > 86400 { $2 = ""; $3 = ""; print }')"
fi

# 12. Force-pushes on GitHub.
if [ -n "$GITHUB" ]; then
    if command -v gh > /dev/null; then
        emit REVIEW "force-push on $GITHUB" \
            "$(gh api "repos/$GITHUB/activity?activity_type=force_push&per_page=30" \
                --jq '.[] | "\(.timestamp) \(.actor.login // "?") \(.ref) \(.before[0:8])->\(.after[0:8])"' 2>&1)"
    else
        echo "      (gh not found: force-pushes not checked)"
    fi
fi

echo "result: $HITS hit(s), $REVIEWS item(s) to review"
[ "$HITS" -gt 0 ] && exit 2
[ "$REVIEWS" -gt 0 ] && exit 1
exit 0
