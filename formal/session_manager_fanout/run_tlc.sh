#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAR_PATH="${ROOT_DIR}/.tools/tla2tools.jar"

mkdir -p "${ROOT_DIR}/.tools"

if [[ ! -f "${JAR_PATH}" ]]; then
  curl -fsSL \
    -o "${JAR_PATH}" \
    "https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar"
fi

run_cfg() {
  local cfg="$1"
  echo
  echo "=== Running ${cfg} ==="
  java -cp "${JAR_PATH}" tlc2.TLC \
    -cleanup \
    -deadlock \
    -config "${cfg}" \
    SessionManagerFanout.tla
}

cd "${ROOT_DIR}"
run_cfg SessionManagerFanout.current.cfg || true
run_cfg SessionManagerFanout.fixed.cfg
