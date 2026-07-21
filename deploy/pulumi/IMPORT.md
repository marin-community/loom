# Importing an existing scripted deployment

Import every existing resource before the first `pulumi up`. Imports only adopt
state; they do not restart the VM. Substitute the names used with
`bootstrap.py` (the defaults below assume instance `loom`, service account
`loom-vm`, repository `loom`, and secret `LOOM_DOTENV`).

```sh
export PROJECT=my-project REGION=us-central1 ZONE=us-central1-a
export VM_SERVICE_ACCOUNT=loom-vm
cd deploy/pulumi

pulumi import gcp:serviceaccount/account:Account loom-vm \
  "projects/${PROJECT}/serviceAccounts/${VM_SERVICE_ACCOUNT}@${PROJECT}.iam.gserviceaccount.com"
pulumi import gcp:compute/firewall:Firewall loom-web \
  "projects/${PROJECT}/global/firewalls/loom-allow-web"
pulumi import gcp:compute/firewall:Firewall loom-ssh \
  "projects/${PROJECT}/global/firewalls/loom-allow-ssh"
pulumi import gcp:compute/address:Address loom-address \
  "projects/${PROJECT}/regions/${REGION}/addresses/loom-ip"
pulumi import gcp:compute/disk:Disk loom-data \
  "projects/${PROJECT}/zones/${ZONE}/disks/loom-data"
pulumi import gcp:secretmanager/secret:Secret loom-dotenv \
  "projects/${PROJECT}/secrets/LOOM_DOTENV"
pulumi import gcp:artifactregistry/repository:Repository loom-images \
  "projects/${PROJECT}/locations/${REGION}/repositories/loom"
pulumi import gcp:compute/instance:Instance loom \
  "projects/${PROJECT}/zones/${ZONE}/instances/loom"
```

Set `loom-gcp:vmServiceAccountName` to `VM_SERVICE_ACCOUNT` before previewing;
the setting exists specifically so a deployment created with
`bootstrap.py --service-account-name` can be adopted without replacement.
Set `loom-gcp:network` to the VM's existing VPC name before previewing. The
stack adds its own tagged rules but deliberately leaves unrelated operator-owned
firewall policy alone.

If `dnsManagedZone` names a Cloud DNS zone that already contains the live A
record, import that record too. The record name is fully qualified and ends in
a dot:

```sh
export DNS_ZONE=example-com DOMAIN=loom.example.com
pulumi import gcp:dns/recordSet:RecordSet loom-dns-address \
  "projects/${PROJECT}/managedZones/${DNS_ZONE}/rrsets/${DOMAIN}./A"
```

When DNS is hosted outside Cloud DNS, leave `dnsManagedZone` unset and there is
no DNS resource to import.

If Artifact Registry was never enabled, omit that import and let Pulumi create
it. A legacy installation with `DATA_DISK_SIZE=0` needs a migration, not a bare
update: take an online database backup, create the data disk, stop the stack,
move Docker's data root (or restore the database and other durable volumes), and
only then attach it under Pulumi. Otherwise the new empty Docker data root makes
the old boot-disk volumes appear to vanish. Do not import old secret versions:
setting `loomDotenv` creates a fresh version while preserving history.
The snapshot policy, backup bucket/timer, WIF provider, and CI service account
are new and need no import.

Before accepting the update:

1. Run `pulumi preview --diff` and require **zero replacements** of the address,
   data disk, secret, and VM. Reconcile names/config or add any omitted imports
   before proceeding.
2. If using Cloud DNS, move the zone and delegate its nameservers before setting
   `dnsManagedZone`. Otherwise leave it unset and retain the current provider's
   A record.
3. Run `pulumi up`, then `post-up.py`. The driver waits for public DNS, triggers
   one new startup generation, streams only that generation's journal, and
   checks HTTPS.
4. After a verified backup and successful restore drill, retire the legacy
   imperative state file `deploy/gcp/deploy.toml`.
5. The legacy script granted Secret Manager access at project scope. Once the
   new secret-level binding is verified, remove that old project-level binding
   so the VM retains access only to `LOOM_DOTENV`.

Pulumi may propose IAM-member additions and metadata changes on the first
managed update; those are expected. A persistent-disk, address, secret, or VM
replacement is not.
