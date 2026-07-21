# Provisioning loom with Pulumi

This stack is the authoritative description of a single-host GCP loom. It
creates the VM and its supporting identity, network, storage, backup, image,
and GitHub Actions resources; [`post-up.py`](post-up.py) performs the few
bring-up checks that are observations rather than infrastructure.

The legacy [`../gcp/bootstrap.py`](../gcp/bootstrap.py) remains temporarily for
existing installations. Do not run it and Pulumi against the same resources
until those resources have been [imported](IMPORT.md).

## One-time state backend

Pulumi cannot create the bucket that contains the state of the update creating
that bucket. Bootstrap a dedicated versioned state bucket once (separate from
the runtime backup bucket), then use it for every operator and CI invocation:

```sh
export PROJECT=my-project
export STATE_BUCKET="${PROJECT}-loom-pulumi-state"
gcloud storage buckets create "gs://${STATE_BUCKET}" \
  --project="${PROJECT}" --location=us-central1 --uniform-bucket-level-access
gcloud storage buckets update "gs://${STATE_BUCKET}" \
  --versioning --public-access-prevention
pulumi login "gs://${STATE_BUCKET}"
```

Restrict bucket IAM to loom infrastructure operators and enable an organization
retention policy appropriate for your recovery requirements. This state holds
encrypted secrets and resource identifiers; the separate `<project>-loom-backups`
bucket holds database backups and is writable only by the VM service account.

## Configure and create

```sh
cd deploy/pulumi
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
pulumi stack init production \
  --secrets-provider='gcpkms://projects/<project>/locations/<location>/keyRings/<ring>/cryptoKeys/<key>'
pulumi config set gcp:project my-project
pulumi config set region us-central1
pulumi config set domain loom.example.com
pulumi config set operatorCidr 203.0.113.7/32
# See Pulumi.example.yaml for the optional settings.

# The complete rendered dotenv is stored as an encrypted Pulumi configuration
# value, then written as a new Secret Manager version by the stack.
pulumi config set --secret loomDotenv "$(loom config render-env --out -)"
pulumi up
./post-up.py --stack production
```

The KMS key in that command must already exist; the GCS state bucket stores only
ciphertext. Grant decrypt permission only to infrastructure operators. If using
the passphrase provider instead, keep `PULUMI_CONFIG_PASSPHRASE` in an access-
controlled secrets manager and test recovery from a fresh workstation. Losing
either the KMS key or passphrase makes the encrypted stack configuration
unrecoverable.

Set `dnsManagedZone` only to the name of an **existing Cloud DNS managed zone
whose nameservers are already delegated at the registrar**. Pulumi manages the
A record but deliberately does not create or delegate a DNS zone. When DNS is
hosted elsewhere, omit the setting, create an A record for the exported
`address`, and only then run `post-up.py`. Both the startup script and post-up
driver refuse to start Caddy until public DNS resolves to the reserved address,
preventing repeated failed ACME challenges.

The stack uses the configured existing VPC (`default` unless `network` is set).
It adds tagged Loom web and SSH rules, with Loom's SSH rule scoped to
`operatorCidr`; it deliberately does not inspect, remove, or override unrelated
firewall rules owned by the operator.

The protected address, data disk, secret, and backup bucket make an accidental
`pulumi destroy` fail rather than erase durable state. Remove protection only
during an explicitly planned teardown. The boot disk is disposable; the
separately attached data disk is retained when the VM is replaced.

## Backups

Two independent recovery mechanisms are installed:

- A daily Compute Engine snapshot policy retains data-disk snapshots for 14
  days by default.
- `loom-backup.timer` runs nightly on the VM. It invokes SQLite's online
  `.backup` API inside the loom container, runs `PRAGMA quick_check`, compresses
  the result, and uploads it to the versioned backup bucket. Objects expire
  after 30 days by default.

Inspect or run the portable backup manually with:

```sh
gcloud compute ssh loom --zone=us-central1-a \
  --command='systemctl status loom-backup.timer; sudo systemctl start loom-backup.service'
```

Restore only with the stack fully stopped. After downloading and decompressing
a selected object on the VM, validate it first, then use the loom image as root
so the named volume, configured app UID/GID, and file mode are handled
correctly:

```sh
cd /opt/loom/deploy/standalone
docker compose run --rm --no-deps --user root --entrypoint sqlite3 \
  -v /path/to/restored.sqlite:/restore/weaver.db:ro loom \
  /restore/weaver.db 'PRAGMA quick_check;'  # must print: ok
docker compose down
docker compose run --rm --no-deps --user root --entrypoint sh \
  -v /path/to/restored.sqlite:/restore/weaver.db:ro loom -ceu '
    state=/home/app/.weaver
    saved="$state/pre-restore-$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$saved"
    for file in weaver.db weaver.db-wal weaver.db-shm; do
      if [ -e "$state/$file" ]; then mv "$state/$file" "$saved/$file"; fi
    done
    install -o app -g app -m 0600 /restore/weaver.db "$state/weaver.db"
  '
docker compose up -d
LOOM_DOMAIN="$(curl -fsS -H 'Metadata-Flavor: Google' \
  http://metadata.google.internal/computeMetadata/v1/instance/attributes/loom-domain)"
curl -fsS "https://${LOOM_DOMAIN}/api/health"  # must print: ok
```

Moving the old DB and its WAL/SHM sidecars together prevents stale WAL pages
from being replayed into the restored database and keeps a rollback copy in the
volume. Do not restore over a running server.

## Deployment images

Pulumi creates a repository-scoped Workload Identity Federation provider and a
service account that can only write Artifact Registry images. Configure these
GitHub repository variables from `pulumi stack output`:

| Variable | Value |
|---|---|
| `LOOM_GCP_PROJECT` | GCP project id |
| `LOOM_GCP_REGION` | stack region |
| `LOOM_GCP_WIF_PROVIDER` | `githubWorkloadIdentityProvider` output |
| `LOOM_GCP_IMAGE_SERVICE_ACCOUNT` | `githubServiceAccount` output |

On pushes to `main`, [the image workflow](../../.github/workflows/image.yml)
uses GitHub OIDC—no JSON key—to publish an immutable commit-SHA tag. The
repository rejects tag replacement and deliberately has no mutable `latest`.
Rerunning the workflow for an already-published commit detects the existing tag
and exits successfully without rebuilding or moving it.
Set `imageMode: pull`, pin `imageTag` to that SHA, and run `pulumi up` for a
reproducible rollout; then run `post-up.py` to stream the new startup
generation.

The example stack uses `imageMode: build` so the first deployment is
self-contained. Once that `pulumi up` has created WIF and Artifact Registry,
set the four repository variables, manually dispatch **Publish deployment
image**, and wait for it to finish. Then change `imageMode` to `pull`, set
`imageTag` to the published commit SHA, run `pulumi up`, and run `post-up.py`.
If a pull-mode VM boots before its selected image exists it fails safely and
the post-up retrigger completes it after the image is published.

## Validation

```sh
python3 -m compileall -q infrastructure.py post-up.py tests
shellcheck ../gcp/startup-script.sh ../gcp/backup-sqlite.sh
actionlint ../../.github/workflows/image.yml
.venv/bin/pip install -r requirements-dev.txt
.venv/bin/python -m pytest -q tests  # Pulumi mocks; no cloud credentials
pulumi preview                       # final provider/schema check
```
