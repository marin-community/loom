#!/usr/bin/env bash
#
# GCE startup-script for the loom standalone-stack VM. Set as instance
# metadata by bootstrap.py; GCE runs this as root on every boot (not just the
# first), so every step below is idempotent.
#
# Reads deploy-placement metadata (repo-url, git-ref, image-mode, ar-image —
# set by bootstrap.py) and fetches loom's whole rendered config as ONE blob
# from Secret Manager (the LOOM_DOTENV secret, pushed by secrets.py via
# `loom config render-env`), then brings up ../standalone/docker-compose.yml
# unmodified. No config field name is hardcoded here — the `loom` binary
# lives on the workstation that ran secrets.py, not on this VM host, so this
# script can't and doesn't parse or assemble config fields itself.
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
# Docker's signed apt repo, not `curl | sh` — this is a long-lived
# internet-facing host, so root shouldn't run an unauthenticated remote
# script. Same keyring idiom as the google-cloud-cli install below.
if ! command -v docker >/dev/null 2>&1; then
  echo "== installing Docker =="
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/debian/gpg \
    -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  # shellcheck disable=SC1091 # /etc/os-release is a Debian-image-provided file, not repo-tracked
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    >/etc/apt/sources.list.d/docker.list
  apt-get update
  apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
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
  apt-get install -y --no-install-recommends google-cloud-cli
fi

# ---- git (to clone the repo) ----------------------------------------------
# Installed in its own command-guarded block, NOT bundled into the gcloud
# install above: on a reboot where gcloud is already present that block is
# skipped, so a git bundled there would never install and the clone below
# would fail with `git: command not found`.
if ! command -v git >/dev/null 2>&1; then
  echo "== installing git =="
  apt-get update
  apt-get install -y --no-install-recommends git
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
  # Grow the filesystem to fill the device — a no-op unless the disk was resized
  # larger (`gcloud compute disks resize`) since the last boot. This makes
  # enlarging the data disk a resize + reboot, with no manual resize2fs; ext4
  # grows online, so the mount above stays put.
  resize2fs "$DATA_DISK_DEVICE" 2>/dev/null || true
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

# ---- get the repo, at the exact code this boot should run -----------------
# Re-run on every boot (not just clone-if-missing), so re-triggering the
# startup script (see ../README.md "Operations") actually picks up a changed
# GIT_REF/REPO_URL instead of silently keeping whatever was checked out first.
if [ ! -d "$REPO_DIR/.git" ]; then
  echo "== cloning ${REPO_URL}@${GIT_REF} =="
  git clone --branch "$GIT_REF" --depth 1 "$REPO_URL" "$REPO_DIR"
else
  echo "== updating ${REPO_DIR} to ${REPO_URL}@${GIT_REF} =="
  git -C "$REPO_DIR" remote set-url origin "$REPO_URL"
  git -C "$REPO_DIR" fetch --depth 1 origin "$GIT_REF"
  git -C "$REPO_DIR" checkout --force FETCH_HEAD
  git -C "$REPO_DIR" clean -fd
fi

ENV_FILE="${REPO_DIR}/deploy/standalone/.env"

echo "== writing ${ENV_FILE} =="
umask 077
gcloud secrets versions access latest --project="$PROJECT" --secret=LOOM_DOTENV >"$ENV_FILE"
# LOOM_IMAGE is deploy-placement, not a loom config field, so it isn't in the
# fetched blob — append it ourselves in pull mode.
if [ "$IMAGE_MODE" = "pull" ] && [ -n "$AR_IMAGE" ]; then
  echo "LOOM_IMAGE=${AR_IMAGE}" >>"$ENV_FILE"
fi
# Docker-out-of-Docker: the loom container mounts this host's Docker socket (see
# ../standalone/docker-compose.yml) so sessions can `docker build`. The non-root
# app user reaches the root-owned socket by joining the host `docker` group, so
# pass its numeric gid through .env — also deploy-placement, not a config field.
# Docker is installed above, so the group exists by now.
DOCKER_GID="$(getent group docker | cut -d: -f3)"
if [ -n "$DOCKER_GID" ]; then
  echo "DOCKER_GID=${DOCKER_GID}" >>"$ENV_FILE"
fi
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
