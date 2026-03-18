#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAR_PATH="${ROOT_DIR}/.tools/tla2tools.jar"
MODE="all"

usage() {
  cat <<'EOF'
Usage:
  ./formal/device_registration_policy/run_tlc.sh [--mode all|ci]

Modes:
  all  Run explanatory failing configs first, then pass-expected configs.
  ci   Run only pass-expected configs; fail on any error.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ "${MODE}" != "all" && "${MODE}" != "ci" ]]; then
  echo "Invalid mode: ${MODE}" >&2
  usage
  exit 1
fi

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
    DeviceRegistrationPolicy.tla
}

cd "${ROOT_DIR}"

if [[ "${MODE}" == "all" ]]; then
  if run_cfg DeviceRegistrationPolicy.current.cfg; then
    echo "Expected DeviceRegistrationPolicy.current.cfg to fail, but it passed." >&2
    exit 1
  else
    echo "DeviceRegistrationPolicy.current.cfg failed as expected."
  fi

  if run_cfg DeviceRegistrationPolicy.bootstrap.current.cfg; then
    echo "Expected DeviceRegistrationPolicy.bootstrap.current.cfg to fail, but it passed." >&2
    exit 1
  else
    echo "DeviceRegistrationPolicy.bootstrap.current.cfg failed as expected."
  fi
fi

run_cfg DeviceRegistrationPolicy.fixed.cfg
run_cfg DeviceRegistrationPolicy.recovery.pass.cfg
