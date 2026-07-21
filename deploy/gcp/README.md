# Legacy scripted GCE deployment

This imperative path is retained for existing installations. New deployments
should use the [Pulumi stack](../pulumi), which adds declarative state,
protected resources, scheduled snapshots and backups, Cloud DNS records, and
keyless image publishing. Follow its [import plan](../pulumi/IMPORT.md) before
letting Pulumi manage resources created here; never run both provisioners over
the same resources independently.

Script-based provisioning of one Google Compute Engine VM that
runs the [standalone stack](../standalone) — loom + its bundled Caddy
front-door — and fronts it with your domain's TLS. This is Model 1 (a single
host with Docker on 80/443); see [`../README.md`](../README.md#future-cloud--cluster)
for why a cluster/Cloud Run deploy is out of scope.

Read [`../README.md`](../README.md) first — it documents the stack itself
(what each service does, the env reference, security posture, first-run
login, wiring the `@loom` GitHub trigger). This document only covers standing
up the GCE VM underneath it.

## Prerequisites

- A GCP project with billing enabled, and the `gcloud` CLI authed
  (`gcloud auth login`) with permission to enable APIs, create service
  accounts, firewall rules, addresses, disks, and instances.
- [`uv`](https://docs.astral.sh/uv/) on your workstation — `bootstrap.py` and
  `secrets.py` are `uv run --script` scripts (self-contained, no venv setup;
  `uv` fetches `click` on first run).
- A domain you control, able to add an `A` record.
- The `loom` binary on your workstation (not the VM — see
  ["Credential handoff"](#credential-handoff)), and `loom.toml` populated by
  `loom setup github-app` and `loom setup secrets` — see
  [`../README.md` "First-run login"](../README.md#first-run-login).

## Credential handoff

`loom.toml` (the typed config the shared `loom config` commands read and
write — see `loom config --help`) is the single source of truth for every
credential this deploy needs. `secrets.py` never hardcodes a field name: it
runs `loom config render-env` and pushes the rendered result straight to
Secret Manager as one blob, `LOOM_DOTENV`. `startup-script.sh` fetches that
one secret and writes it to `deploy/standalone/.env` on the VM verbatim — the
VM host never runs `loom` itself (only the container image has the binary)
and never sees an individual field name either. Adding a config field is
entirely a `crates/loom` change; nothing here needs updating for it.

GCP infra placement — `PROJECT`, `REGION`, `ZONE`, `MACHINE_TYPE`,
`LOOM_DOMAIN` (for the DNS-wait gate below), ... — is a separate,
deploy-specific concern and is **not** part of that handoff: it stays
`bootstrap.py` flags/env vars and instance metadata, the same way it would
for any other host you chose to run the stack on. (`LOOM_DOMAIN` is *also* a
`loom.toml` field the app itself reads — the two are naturally the same
value, kept in sync by you, not by any code path here.)

Every `loom.toml` field is optional (an unset field is simply absent from the
rendered blob and skipped by `push-secrets` — no failure, no placeholder).
`anthropic_api_key` in particular is usually one you *don't* set: the more
common path is skipping it in `loom setup secrets` and logging in to Claude
interactively once the VM is up, the same as any other host —
`gcloud --project=my-project compute ssh loom --zone=us-central1-a` in, then
`cd /opt/loom/deploy/standalone && sudo docker compose exec loom claude`.

## Run order

The ordering constraint that matters: **the static IP must exist and DNS must
resolve to it before the stack starts**, or Caddy's ACME HTTP-01 challenge
fails and no certificate issues (Let's Encrypt is rate-limited, so repeated
failures are expensive to retry). `bootstrap.py` enforces this — it reserves
the IP, prints the exact record to set, and then polls for DNS to resolve
before it creates the VM.

```sh
# 0. On your workstation (not the VM — these are daemon-less, no running loom
#    needed yet): populate loom.toml.
loom setup github-app --base-url https://loom.example.com
loom setup secrets

cd deploy/gcp

# 1. Render loom.toml and push it to Secret Manager as one blob, for
#    startup-script.sh to fetch on boot.
PROJECT=my-project ./secrets.py

# 2. Provision the static IP, firewall, service account, and VM. Prints the
#    DNS A record to set, then waits for it to resolve before creating the VM.
PROJECT=my-project LOOM_DOMAIN=loom.example.com ./bootstrap.py
```

While `bootstrap.py` is waiting, go set the `A` record it printed at your DNS
provider. Once it resolves, the script creates the VM. GCE runs
`startup-script.sh` as the instance's `startup-script` metadata: it installs
Docker, git, and the `gcloud` CLI, fetches the `LOOM_DOTENV` blob from Secret
Manager and writes it to `deploy/standalone/.env`, clones the repo, and runs
`docker compose up -d --build` from `deploy/standalone` — unmodified.

`bootstrap.py` then drives the deploy to completion for you: it waits for SSH,
streams the startup-script's `journalctl` output until it finishes (or fails),
and polls `https://<LOOM_DOMAIN>/` once the stack is up — so a single run ends
either at "loom is live" or a specific failure. You don't need to tail the boot
yourself, but to watch a later reboot:

```sh
gcloud --project=my-project compute ssh loom --zone=us-central1-a \
  --command='sudo journalctl -u google-startup-scripts -f'
```

Both scripts are check-before-create, so re-running either after an
interruption is safe. Re-running `bootstrap.py` against an existing VM updates
its `startup-script` + placement metadata in place and re-triggers the boot to
apply it (and pushes `LOOM_DOTENV` via `secrets.py` first if that secret is
still missing) — so a fixed startup script or a new `--git-ref` rolls out with a
bare re-run. It will *not* recreate or resize an existing instance, though;
delete it first to change its machine type or disks. Re-run `secrets.py`
yourself any time `loom.toml` changes, then re-run `bootstrap.py` (or re-trigger
the startup script, see ["Operations"](#operations)) to pick it up.

Run `./bootstrap.py --help` and `./secrets.py --help` for the full list of
options and their defaults — every `bootstrap.py` option also has a
same-named env var (`PROJECT`, `LOOM_DOMAIN`, `MACHINE_TYPE`, ...), so the
env-var invocations above and `--flag` invocations are interchangeable.

After a successful run `bootstrap.py` writes the knobs it used to a gitignored
`deploy/gcp/deploy.toml` (project, domain, region, machine type, disk sizes,
...), and reads them back as defaults next time — so a bare `./bootstrap.py`
re-deploys with the last settings, and you only pass a flag to *change* one.
Precedence is flag > env var > `deploy.toml` > built-in default; `push-image.py`
reads the same file for its `--project`/`--region`. This is deploy/infra state,
deliberately separate from `loom.toml` (the app config that `render-env` bakes
into the container's env and Secret Manager).
`secrets.py --granular` pushes each secret field to its own Secret Manager
secret instead of the one `LOOM_DOTENV` blob, for independent rotation — see
`./secrets.py --help`; the shipped `startup-script.sh` expects the blob.

## Manual-once checklist

These steps aren't scriptable (they happen in GitHub's UI, or need a human in
a browser) and only need doing once per deploy:

1. **Run `loom setup github-app` and `loom setup secrets`** — step 0 of
   ["Run order"](#run-order) above; see also
   [`../README.md` "First-run login"](../README.md#first-run-login).
2. **First login** — open `https://<LOOM_DOMAIN>` and *Continue with GitHub*
   as the `LOOM_OWNER_GITHUB` account. See
   [`../README.md` "First-run login"](../README.md#first-run-login).
3. **Install the App on your repos** — `loom setup github-app` prints
   `https://github.com/apps/<app-slug>/installations/new`; installing it both
   wires the `@loom` trigger's webhook and allowlists the repo for cloning, no
   separate webhook or `curl` registration needed. See
   [`../README.md` "Wire the @loom GitHub trigger"](../README.md#wire-the-loom-github-trigger).

## Durable state

The compose file's named volumes (`loom_home` — the sqlite DB, machine token,
`~/.claude.json`, the managed repo store; `caddy_data` — issued TLS certs) are
Docker volumes, which live under Docker's data-root on disk. **Deleting the
VM deletes them along with it** — you'd lose the database and have to
re-issue a certificate (subject to Let's Encrypt's rate limits).

`bootstrap.py` mitigates this by default: it creates a separate persistent
disk (`DATA_DISK_SIZE`, default 500GB) and `startup-script.sh` points Docker's
entire data-root at it (`/etc/docker/daemon.json`), so every named volume
lands on that disk without any change to `docker-compose.yml`. The images and
layers agents build with in-session `docker build` land there too — the loom
container shares this host daemon over its socket (see
[`../README.md` "Agent runtime"](../README.md#agent-runtime--client-packages)),
so `docker system df` on the VM and the 500GB default both cover your build
footprint. That disk still gets deleted if you delete the VM *and* the disk
together — it is durability against VM recreation, not against
`terraform destroy`-style teardown. To keep state across a full teardown, detach the disk first
(`gcloud compute instances detach-disk`) or snapshot it
(`gcloud compute disks snapshot`) before deleting the instance.

**Resizing the data disk.** Re-running `bootstrap.py` with a larger
`--data-disk-size` does *not* touch an existing disk — it's created only when
absent, so a re-run just logs "already exists" and moves on. To grow it in
place, resize the disk (GCP grows live; you can't shrink) and reboot:

```sh
gcloud compute disks resize loom-data --size=500GB --zone=<zone>
gcloud compute instances reset loom --zone=<zone>
```

`startup-script.sh` runs `resize2fs` on every boot, so the ext4 filesystem
expands to fill the enlarged disk automatically — no manual filesystem step.

Set `DATA_DISK_SIZE=0` to skip this and put everything on the boot disk
instead — simpler, but the loss-on-VM-delete risk above applies to the whole
stack, not just a separate disk.

## Build once, pull many

The default (`IMAGE_MODE=build`) has the VM build the image itself with
`docker compose up -d --build` — the repo-root `Dockerfile` is a full
Rust+Node build, so this is slow and the default machine type
(`e2-standard-4`, 100GB boot disk) is sized to avoid OOMing during it.

For faster boots and re-deploys, build the image once on your workstation and
push it to Artifact Registry, then point the VM at the prebuilt image.
`push-image.py` does the whole push side — enables the AR API, creates the
`loom` repo, wires `docker` auth, and builds+pushes for `linux/amd64`:

```sh
PROJECT=my-project ./push-image.py
```

It pushes to `<region>-docker.pkg.dev/<project>/loom/loom:latest` — the exact
path `bootstrap.py --image-mode=pull` derives by default, so you don't repeat
it:

```sh
IMAGE_MODE=pull PROJECT=my-project LOOM_DOMAIN=loom.example.com ./bootstrap.py
```

(`AR_IMAGE`/`--ar-image` is only needed to pull some *other* image.) It builds
`release` for `linux/amd64`; on a non-amd64 workstation that needs an emulating
`docker-container` buildx builder (`docker buildx create --use`).

`startup-script.sh` then does `gcloud auth configure-docker` (using the VM's
own service account) and `docker compose pull` instead of `--build`. To roll
out a new build to an existing VM, `./push-image.py` again, then either
`docker compose pull && docker compose up -d` over SSH, or reset the instance
to re-run the startup script.

## Operations

```sh
# SSH in (only your bootstrap-time OPERATOR_IP is allowed on :22)
gcloud --project=my-project compute ssh loom --zone=us-central1-a

# on the VM
cd /opt/loom/deploy/standalone
sudo docker compose ps
sudo docker compose logs -f loom
sudo docker compose logs -f caddy    # watch cert issuance

# re-run the startup script without a full reboot
sudo google_metadata_script_runner startup
```

7878 (loom's own port) is never opened on the VM's firewall — the only way in
is through Caddy on 80/443/443-udp, enforced by `bootstrap.py`'s firewall
rules.
