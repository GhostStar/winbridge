#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

EXTRA_ARGS=()
if [[ "${1:-}" == "--disable-sandbox" ]]; then
    EXTRA_ARGS+=(--disable-sandbox)
fi

CLT_FRAMEWORKS="/Library/Developer/CommandLineTools/Library/Developer/Frameworks"
CLT_USRLIB="/Library/Developer/CommandLineTools/Library/Developer/usr/lib"

if [[ -d "$CLT_FRAMEWORKS/Testing.framework" ]]; then
    swift test ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} \
        --disable-xctest \
        --enable-swift-testing \
        -Xswiftc -F -Xswiftc "$CLT_FRAMEWORKS" \
        -Xlinker -F -Xlinker "$CLT_FRAMEWORKS" \
        -Xlinker -rpath -Xlinker "$CLT_FRAMEWORKS" \
        -Xlinker -rpath -Xlinker "$CLT_USRLIB"
else
    swift test ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}
fi
