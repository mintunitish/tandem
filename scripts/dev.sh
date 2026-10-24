#!/usr/bin/env bash
# Run the debug build of Tandem on a project: scripts/dev.sh [path] (default: current dir).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/app}"
PROJECT="$(cd "${1:-$PWD}" && pwd)"
cd "$ROOT"
exec cargo run -p tandem-app --bin tandem -- "$PROJECT"
