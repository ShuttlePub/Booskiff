#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER_LOG="${ROOT_DIR}/e2e/core-server.log"
HEALTH_TIMEOUT_SECONDS=90

cleanup() {
  local status=$?
  if (( status != 0 )); then
    echo "E2E failed (exit ${status}); core server log follows:" >&2
    if [[ -f "${SERVER_LOG}" ]]; then
      tail -n 200 "${SERVER_LOG}" >&2
    else
      echo "(core server log was not created)" >&2
    fi
  fi
  if [[ "${BOOSKIFF_E2E_TEARDOWN:-0}" == "1" ]]; then
    docker compose -f "${ROOT_DIR}/compose.yml" down -v
  fi
  exit "${status}"
}
trap cleanup EXIT

cd "${ROOT_DIR}"
: >"${SERVER_LOG}"

docker compose up -d

deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
while true; do
  postgres_id="$(docker compose ps -q postgres)"
  minio_id="$(docker compose ps -q minio)"
  mc_init_id="$(docker compose ps -aq mc-init)"

  postgres_health="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "${postgres_id}" 2>/dev/null || true)"
  minio_health="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "${minio_id}" 2>/dev/null || true)"
  mc_init_state="$(docker inspect --format '{{.State.Status}}:{{.State.ExitCode}}' "${mc_init_id}" 2>/dev/null || true)"

  if [[ "${postgres_health}" == "healthy" && "${minio_health}" == "healthy" && "${mc_init_state}" == "exited:0" ]]; then
    break
  fi
  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for compose services: postgres=${postgres_health}, minio=${minio_health}, mc-init=${mc_init_state}" >&2
    docker compose logs --tail=200 >&2
    exit 1
  fi
  sleep 1
done

cargo build -p core
cargo test -p core --test e2e -- --ignored --test-threads=1
