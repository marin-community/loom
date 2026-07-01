# Deploying loom to a single GCE VM

Script-based (no Terraform) provisioning of one Google Compute Engine VM that
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
- The credential values from [`../README.md` "Required environment"](../README.md#required-environment)
  ready to hand to `secrets.py` — at minimum `GH_TOKEN`, `LOOM_OWNER_GITHUB`,
  and either `ANTHROPIC_API_KEY` or a plan to log in to Claude interactively
  after boot.

## Run order

The ordering constraint that matters: **the static IP must exist and DNS must
resolve to it before the stack starts**, or Caddy's ACME HTTP-01 challenge
fails and no certificate issues (Let's Encrypt is rate-limited, so repeated
failures are expensive to retry). `bootstrap.py` enforces this — it reserves
the IP, prints the exact record to set, and then polls for DNS to resolve
before it creates the VM.

```sh
cd deploy/gcp

# 1. Store the secrets startup-script.sh will fetch on boot. Prompts for each
#    (or export SECRET_NAME=value beforehand for non-interactive use).
PROJECT=my-project ./secrets.py

# 2. Provision the static IP, firewall, service account, and VM. Prints the
#    DNS A record to set, then waits for it to resolve before creating the VM.
PROJECT=my-project LOOM_DOMAIN=loom.example.com ./bootstrap.py
```

While `bootstrap.py` is waiting, go set the `A` record it printed at your DNS
provider. Once it resolves, the script proceeds to create the VM. GCE runs
`startup-script.sh` as the instance's `startup-script` metadata: it installs
Docker, installs the `gcloud` CLI, fetches the secrets from Secret Manager,
clones the repo, and runs `docker compose up -d --build` from
`deploy/standalone` — unmodified.

Watch it boot:

```sh
gcloud --project=my-project compute ssh loom --zone=us-central1-a \
  --command='sudo journalctl -u google-startup-scripts -f'
```

Both scripts are check-before-create, so re-running either after an
interruption (or to pick up a new secret value) is safe. `bootstrap.py` will
not recreate or resize an existing instance — delete it first if you need to
change its machine type or disks.

Run `./bootstrap.py --help` and `./secrets.py --help` for the full list of
configuration options (region, zone, machine type, disk sizes, image source,
...) and their defaults — every option also has a same-named env var
(`PROJECT`, `LOOM_DOMAIN`, `MACHINE_TYPE`, ...), so the env-var invocations
above and `--flag` invocations are interchangeable.

## Manual-once checklist

These steps aren't scriptable (they happen in GitHub's UI, or need a human in
a browser) and only need doing once per deploy:

1. **Register a GitHub OAuth app** — callback URL
   `https://<LOOM_DOMAIN>/api/auth/github/callback`. Put its client
   ID/secret in Secret Manager via `./secrets.py LOOM_GITHUB_CLIENT_ID
   LOOM_GITHUB_CLIENT_SECRET`, then either wait for the next boot or
   re-trigger the startup script (`gcloud compute instances reset` or SSH in
   and re-run `sudo google_metadata_script_runner startup`) to pick it up.
2. **First login** — open `https://<LOOM_DOMAIN>` and *Continue with GitHub*
   as the `LOOM_OWNER_GITHUB` account. See
   [`../README.md` "First-run login"](../README.md#first-run-login).
3. **Wire the `@loom` trigger** — register the repo and add the webhook, per
   [`../README.md` "Wire the @loom GitHub trigger"](../README.md#wire-the-loom-github-trigger).

A `loom setup` wizard that walks this manifest-style flow automatically is
planned (see weaver issue #350); until then it's these three manual steps.

## Durable state

The compose file's named volumes (`loom_home` — the sqlite DB, machine token,
`~/.claude.json`, the managed repo store; `caddy_data` — issued TLS certs) are
Docker volumes, which live under Docker's data-root on disk. **Deleting the
VM deletes them along with it** — you'd lose the database and have to
re-issue a certificate (subject to Let's Encrypt's rate limits).

`bootstrap.py` mitigates this by default: it creates a separate persistent
disk (`DATA_DISK_SIZE`, default 50GB) and `startup-script.sh` points Docker's
entire data-root at it (`/etc/docker/daemon.json`), so every named volume
lands on that disk without any change to `docker-compose.yml`. That disk
still gets deleted if you delete the VM *and* the disk together — it is
durability against VM recreation, not against `terraform destroy`-style
teardown. To keep state across a full teardown, detach the disk first
(`gcloud compute instances detach-disk`) or snapshot it
(`gcloud compute disks snapshot`) before deleting the instance.

Set `DATA_DISK_SIZE=0` to skip this and put everything on the boot disk
instead — simpler, but the loss-on-VM-delete risk above applies to the whole
stack, not just a separate disk.

## Build once, pull many

The default (`IMAGE_MODE=build`) has the VM build the image itself with
`docker compose up -d --build` — the repo-root `Dockerfile` is a full
Rust+Node build, so this is slow and the default machine type
(`e2-standard-4`, 100GB boot disk) is sized to avoid OOMing during it.

For faster boots and re-deploys, build once and push to Artifact Registry,
then point the VM at the prebuilt image:

```sh
gcloud artifacts repositories create loom --repository-format=docker \
  --location=us-central1 --project=my-project

gcloud auth configure-docker us-central1-docker.pkg.dev

docker build -t us-central1-docker.pkg.dev/my-project/loom/loom:latest .
docker push us-central1-docker.pkg.dev/my-project/loom/loom:latest
```

Then run `bootstrap.py` with:

```sh
IMAGE_MODE=pull \
AR_IMAGE=us-central1-docker.pkg.dev/my-project/loom/loom:latest \
PROJECT=my-project LOOM_DOMAIN=loom.example.com ./bootstrap.py
```

`startup-script.sh` then does `gcloud auth configure-docker` (using the VM's
own service account) and `docker compose pull` instead of `--build`. To roll
out a new build to an existing VM, push a new image tag and either
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
