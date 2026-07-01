#!/usr/bin/env bash
#
# GCE startup-script for the loom standalone-stack VM. Set as instance
# metadata by bootstrap.py; GCE runs this as root on every boot (not just the
# first), so every step below is idempotent.
#
# Reads non-secret config from instance metadata (loom-domain, repo-url,
# git-ref, image-mode, ar-image — set by bootstrap.py) and secret values from
# Secret Manager (set by secrets.py), then brings up
# ../standalone/docker-compose.yml unmodified.
#
# Logs to the serial console and journalctl (GCE captures startup-script
# stdout/stderr under the google-startup-scripts unit) — no extra logging
# setup needed here.
#
# Bash, not click/uv like bootstrap.py and secrets.py: this runs as the very
# first thing on a fresh VM boot, before Docker, gcloud, or uv exist — a
# Python rewrite would need to bootstrap uv first, adding fragility for no
# benefit on a boot-critical path.
set -euo pipefail

META="http://metadata.google.internal/computeMetadata/v1"
meta() { curl -sf -H "Metadata-Flavor: Google" "${META}/$1"; }

PROJECT="$(meta project/project-id)"
LOOM_DOMAIN="$(meta instance/attributes/loom-domain)"
REPO_URL="$(meta instance/attributes/repo-url)"
GIT_REF="$(meta instance/attributes/git-ref)"
IMAGE_MODE="$(meta instance/attributes/image-mode)"
AR_IMAGE="$(meta instance/attributes/ar-image 2>/dev/null || true)"

REPO_DIR=/opt/loom
DATA_DISK_DEVICE=/dev/disk/by-id/google-loom-data
DATA_MOUNT=/mnt/loom-data

echo "== loom startup-script: domain=${LOOM_DOMAIN} image-mode=${IMAGE_MODE} =="

# ---- Docker + compose plugin ----------------------------------------------
if ! command -v docker >/dev/null 2>&1; then
  echo "== installing Docker =="
  curl -fsSL https://get.docker.com | sh
fi
systemctl enable --now docker

# ---- gcloud CLI (to read Secret Manager) -----------------------------------
if ! command -v gcloud >/dev/null 2>&1; then
  echo "== installing google-cloud-cli =="
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg \
    -o /etc/apt/keyrings/cloud.google.asc
  chmod a+r /etc/apt/keyrings/cloud.google.asc
  echo "deb [signed-by=/etc/apt/keyrings/cloud.google.asc] https://packages.cloud.google.com/apt cloud-sdk main" \
    >/etc/apt/sources.list.d/google-cloud-sdk.list
  apt-get update
  apt-get install -y --no-install-recommends google-cloud-cli git
fi

# ---- optional persistent data disk for loom_home / caddy_data -------------
# Redirects Docker's entire data-root onto the separate disk, so the compose
# file's named volumes (loom_home, caddy_data, ...) land there unmodified —
# no fork of docker-compose.yml needed. See ../README.md "Durable state".
if [ -e "$DATA_DISK_DEVICE" ]; then
  if ! blkid "$DATA_DISK_DEVICE" >/dev/null 2>&1; then
    echo "== formatting data disk (first boot) =="
    mkfs.ext4 -m 0 -F "$DATA_DISK_DEVICE"
  fi
  mkdir -p "$DATA_MOUNT"
  if ! mountpoint -q "$DATA_MOUNT"; then
    mount "$DATA_DISK_DEVICE" "$DATA_MOUNT"
  fi
  if ! grep -q "^${DATA_DISK_DEVICE} " /etc/fstab; then
    echo "${DATA_DISK_DEVICE} ${DATA_MOUNT} ext4 discard,defaults,nofail 0 2" >>/etc/fstab
  fi

  mkdir -p "${DATA_MOUNT}/docker"
  if [ ! -f /etc/docker/daemon.json ] || ! grep -q "$DATA_MOUNT" /etc/docker/daemon.json 2>/dev/null; then
    echo "== pointing Docker data-root at ${DATA_MOUNT}/docker =="
    mkdir -p /etc/docker
    cat >/etc/docker/daemon.json <<EOF
{
  "data-root": "${DATA_MOUNT}/docker"
}
EOF
    systemctl restart docker
  fi
fi

# ---- fetch secrets into deploy/standalone/.env -----------------------------
if [ ! -d "$REPO_DIR/.git" ]; then
  echo "== cloning ${REPO_URL}@${GIT_REF} =="
  git clone --branch "$GIT_REF" --depth 1 "$REPO_URL" "$REPO_DIR"
fi

ENV_FILE="${REPO_DIR}/deploy/standalone/.env"
secret() { gcloud secrets versions access latest --project="$PROJECT" --secret="$1"; }

echo "== writing ${ENV_FILE} =="
umask 077
{
  echo "LOOM_DOMAIN=${LOOM_DOMAIN}"
  echo "LOOM_OWNER_GITHUB=$(secret LOOM_OWNER_GITHUB)"
  echo "GH_TOKEN=$(secret GH_TOKEN)"
  echo "LOOM_GITHUB_WEBHOOK_SECRET=$(secret LOOM_GITHUB_WEBHOOK_SECRET)"
  echo "ANTHROPIC_API_KEY=$(secret ANTHROPIC_API_KEY)"
  echo "LOOM_GITHUB_CLIENT_ID=$(secret LOOM_GITHUB_CLIENT_ID)"
  echo "LOOM_GITHUB_CLIENT_SECRET=$(secret LOOM_GITHUB_CLIENT_SECRET)"
  if [ "$IMAGE_MODE" = "pull" ] && [ -n "$AR_IMAGE" ]; then
    echo "LOOM_IMAGE=${AR_IMAGE}"
  fi
} >"$ENV_FILE"
chmod 600 "$ENV_FILE"

# ---- bring up the stack -----------------------------------------------------
cd "${REPO_DIR}/deploy/standalone"

if [ "$IMAGE_MODE" = "pull" ] && [ -n "$AR_IMAGE" ]; then
  registry="${AR_IMAGE%%/*}"
  gcloud auth configure-docker "$registry" --quiet
  echo "== pulling ${AR_IMAGE} =="
  docker compose pull
  docker compose up -d
else
  echo "== building and starting the stack =="
  docker compose up -d --build
fi

echo "== loom startup-script done =="
