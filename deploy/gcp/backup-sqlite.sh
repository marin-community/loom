#!/usr/bin/env bash
# Create a transactionally consistent loom SQLite backup and upload it to the
# instance's configured GCS bucket. Installed by startup-script.sh and invoked
# by loom-backup.timer; it is also safe to run by hand.
set -euo pipefail

META="http://metadata.google.internal/computeMetadata/v1"
metadata() { curl -fsS -H "Metadata-Flavor: Google" "${META}/$1"; }

PROJECT="$(metadata project/project-id)"
BUCKET="$(metadata instance/attributes/backup-bucket)"
REPO_DIR=/opt/loom
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP_NAME="weaver-${STAMP}.sqlite"
CONTAINER_PATH="/tmp/${BACKUP_NAME}"
TMP_DIR="$(mktemp -d /tmp/loom-backup.XXXXXX)"
HOST_PATH="${TMP_DIR}/${BACKUP_NAME}"

cleanup() {
  docker compose -f "${REPO_DIR}/deploy/standalone/docker-compose.yml" \
    exec -T loom rm -f "$CONTAINER_PATH" >/dev/null 2>&1 || true
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

cd "${REPO_DIR}/deploy/standalone"

# Never copy the live database files directly: WAL state may not be reflected
# in the main file. SQLite's online backup API takes a consistent snapshot while
# loom continues to serve traffic.
docker compose exec -T loom sqlite3 /home/app/.weaver/weaver.db \
  ".timeout 30000" ".backup '${CONTAINER_PATH}'"

check="$(docker compose exec -T loom sqlite3 "$CONTAINER_PATH" \
  "PRAGMA quick_check;" | tr -d '\r')"
if [ "$check" != "ok" ]; then
  echo "loom backup failed integrity check: ${check}" >&2
  exit 1
fi

docker compose cp "loom:${CONTAINER_PATH}" "$HOST_PATH"
gzip "$HOST_PATH"
gcloud storage cp "${HOST_PATH}.gz" \
  "gs://${BUCKET}/sqlite/${PROJECT}/${BACKUP_NAME}.gz"
