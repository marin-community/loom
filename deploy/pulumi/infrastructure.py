"""Declarative Google Cloud resources for a single-host loom deployment.

Keep the resource graph in ``create_infrastructure``: tests can run it under
Pulumi mocks without credentials, while ``__main__.py`` is intentionally only
the stack entry point.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

import pulumi
import pulumi_gcp as gcp


ROOT = Path(__file__).resolve().parents[2]


def _positive_config_int(value: int | None, default: int, name: str) -> int:
    """Default only an absent Pulumi integer; reject explicit zero/negatives."""
    resolved = default if value is None else value
    if resolved <= 0:
        raise ValueError(f"{name} must be positive")
    return resolved


@dataclass(frozen=True)
class DeploymentConfig:
    project: str
    region: str
    zone: str
    domain: str
    operator_cidr: str
    loom_dotenv: pulumi.Input[str]
    dns_managed_zone: str | None = None
    network: str = "default"
    instance_name: str = "loom"
    vm_service_account_name: str = "loom-vm"
    machine_type: str = "e2-highmem-4"
    boot_disk_gb: int = 100
    data_disk_gb: int = 500
    repo_url: str = "https://github.com/rjpower/weaver.git"
    git_ref: str = "main"
    image_mode: str = "build"
    image_tag: str = "latest"
    github_repository: str = "rjpower/weaver"
    github_ref: str = "refs/heads/main"
    snapshot_retention_days: int = 14
    backup_retention_days: int = 30

    def __post_init__(self) -> None:
        if self.image_mode not in {"build", "pull"}:
            raise ValueError("imageMode must be 'build' or 'pull'")
        if self.image_mode == "pull" and not re.fullmatch(
            r"[0-9a-f]{40}", self.image_tag
        ):
            raise ValueError("pull mode requires an immutable commit-SHA imageTag")
        for name, value in (
            ("bootDiskGb", self.boot_disk_gb),
            ("dataDiskGb", self.data_disk_gb),
            ("snapshotRetentionDays", self.snapshot_retention_days),
            ("backupRetentionDays", self.backup_retention_days),
        ):
            _positive_config_int(value, value, name)

    @classmethod
    def from_pulumi(cls) -> "DeploymentConfig":
        config = pulumi.Config()
        gcp_config = pulumi.Config("gcp")
        project = gcp_config.require("project")
        region = config.get("region") or "us-central1"
        return cls(
            project=project,
            region=region,
            zone=config.get("zone") or f"{region}-a",
            domain=config.require("domain"),
            operator_cidr=config.require("operatorCidr"),
            # Pulumi encrypts this in stack state. Render it with
            # `loom config render-env --out -` and set it with --secret.
            loom_dotenv=config.require_secret("loomDotenv"),
            dns_managed_zone=config.get("dnsManagedZone"),
            network=config.get("network") or "default",
            instance_name=config.get("instanceName") or "loom",
            vm_service_account_name=config.get("vmServiceAccountName") or "loom-vm",
            machine_type=config.get("machineType") or "e2-highmem-4",
            boot_disk_gb=_positive_config_int(
                config.get_int("bootDiskGb"), 100, "bootDiskGb"
            ),
            data_disk_gb=_positive_config_int(
                config.get_int("dataDiskGb"), 500, "dataDiskGb"
            ),
            repo_url=config.get("repoUrl")
            or "https://github.com/rjpower/weaver.git",
            git_ref=config.get("gitRef") or "main",
            image_mode=config.get("imageMode") or "build",
            image_tag=config.get("imageTag") or "latest",
            github_repository=config.get("githubRepository") or "rjpower/weaver",
            github_ref=config.get("githubRef") or "refs/heads/main",
            snapshot_retention_days=_positive_config_int(
                config.get_int("snapshotRetentionDays"), 14, "snapshotRetentionDays"
            ),
            backup_retention_days=_positive_config_int(
                config.get_int("backupRetentionDays"), 30, "backupRetentionDays"
            ),
        )


@dataclass(frozen=True)
class Infrastructure:
    address: gcp.compute.Address
    instance: gcp.compute.Instance
    artifact_repository: gcp.artifactregistry.Repository
    backup_bucket: gcp.storage.Bucket
    workload_identity_provider: gcp.iam.WorkloadIdentityPoolProvider


def _enable_apis(project: str) -> list[gcp.projects.Service]:
    services = (
        "artifactregistry.googleapis.com",
        "compute.googleapis.com",
        "dns.googleapis.com",
        "iam.googleapis.com",
        "iamcredentials.googleapis.com",
        "secretmanager.googleapis.com",
        "sts.googleapis.com",
        "storage.googleapis.com",
    )
    return [
        gcp.projects.Service(
            f"api-{service.split('.')[0]}",
            project=project,
            service=service,
            disable_on_destroy=False,
        )
        for service in services
    ]


def create_infrastructure(config: DeploymentConfig) -> Infrastructure:
    """Create loom's GCP resource graph and export its operator-facing values."""
    apis = _enable_apis(config.project)
    api_options = pulumi.ResourceOptions(depends_on=apis)

    vm_account = gcp.serviceaccount.Account(
        "loom-vm",
        project=config.project,
        account_id=config.vm_service_account_name,
        display_name="loom standalone VM",
        opts=api_options,
    )
    ci_account = gcp.serviceaccount.Account(
        "loom-image-ci",
        project=config.project,
        account_id="loom-image-ci",
        display_name="GitHub Actions loom image publisher",
        opts=api_options,
    )

    artifact_repository = gcp.artifactregistry.Repository(
        "loom-images",
        project=config.project,
        location=config.region,
        repository_id="loom",
        format="DOCKER",
        docker_config={"immutable_tags": True},
        description="Immutable loom deployment images",
        opts=api_options,
    )
    vm_image_reader = gcp.artifactregistry.RepositoryIamMember(
        "loom-vm-image-reader",
        project=config.project,
        location=artifact_repository.location,
        repository=artifact_repository.repository_id,
        role="roles/artifactregistry.reader",
        member=vm_account.email.apply(lambda email: f"serviceAccount:{email}"),
    )
    gcp.artifactregistry.RepositoryIamMember(
        "loom-ci-image-writer",
        project=config.project,
        location=artifact_repository.location,
        repository=artifact_repository.repository_id,
        role="roles/artifactregistry.writer",
        member=ci_account.email.apply(lambda email: f"serviceAccount:{email}"),
    )

    web_firewall = gcp.compute.Firewall(
        "loom-web",
        project=config.project,
        network=config.network,
        name=f"{config.instance_name}-allow-web",
        direction="INGRESS",
        source_ranges=["0.0.0.0/0"],
        target_tags=["loom-web"],
        allows=[
            {"protocol": "tcp", "ports": ["80", "443"]},
            {"protocol": "udp", "ports": ["443"]},
        ],
        opts=api_options,
    )
    ssh_firewall = gcp.compute.Firewall(
        "loom-ssh",
        project=config.project,
        network=config.network,
        name=f"{config.instance_name}-allow-ssh",
        direction="INGRESS",
        source_ranges=[config.operator_cidr],
        target_tags=["loom-ssh"],
        allows=[{"protocol": "tcp", "ports": ["22"]}],
        opts=api_options,
    )
    address = gcp.compute.Address(
        "loom-address",
        project=config.project,
        region=config.region,
        name=f"{config.instance_name}-ip",
        opts=pulumi.ResourceOptions(depends_on=apis, protect=True),
    )

    dns_record: gcp.dns.RecordSet | None = None
    if config.dns_managed_zone:
        dns_record = gcp.dns.RecordSet(
            "loom-dns-address",
            project=config.project,
            managed_zone=config.dns_managed_zone,
            name=f"{config.domain.rstrip('.')}.",
            type="A",
            ttl=300,
            rrdatas=[address.address],
            opts=api_options,
        )

    data_disk = gcp.compute.Disk(
        "loom-data",
        project=config.project,
        zone=config.zone,
        name=f"{config.instance_name}-data",
        type="pd-balanced",
        size=config.data_disk_gb,
        opts=pulumi.ResourceOptions(depends_on=apis, protect=True),
    )
    snapshot_policy = gcp.compute.ResourcePolicy(
        "loom-data-snapshots",
        project=config.project,
        region=config.region,
        name=f"{config.instance_name}-data-daily",
        snapshot_schedule_policy={
            "schedule": {
                "daily_schedule": {"days_in_cycle": 1, "start_time": "04:00"}
            },
            "retention_policy": {
                "max_retention_days": config.snapshot_retention_days,
                "on_source_disk_delete": "KEEP_AUTO_SNAPSHOTS",
            },
            "snapshot_properties": {"storage_locations": config.region},
        },
        opts=api_options,
    )
    snapshot_attachment = gcp.compute.DiskResourcePolicyAttachment(
        "loom-data-snapshot-policy",
        project=config.project,
        zone=config.zone,
        disk=data_disk.name,
        name=snapshot_policy.name,
    )

    dotenv_secret = gcp.secretmanager.Secret(
        "loom-dotenv",
        project=config.project,
        secret_id="LOOM_DOTENV",
        replication={"auto": {}},
        opts=pulumi.ResourceOptions(depends_on=apis, protect=True),
    )
    secret_version = gcp.secretmanager.SecretVersion(
        "loom-dotenv-current",
        secret=dotenv_secret.id,
        secret_data=config.loom_dotenv,
        # Secret data changes create a fresh version. Retain superseded versions
        # for rollback without making ordinary rotation fight `protect`.
        opts=pulumi.ResourceOptions(retain_on_delete=True),
    )
    vm_secret_reader = gcp.secretmanager.SecretIamMember(
        "loom-vm-secret-reader",
        project=config.project,
        secret_id=dotenv_secret.secret_id,
        role="roles/secretmanager.secretAccessor",
        member=vm_account.email.apply(lambda email: f"serviceAccount:{email}"),
    )

    backup_bucket = gcp.storage.Bucket(
        "loom-backups",
        project=config.project,
        location=config.region,
        name=pulumi.Output.format(
            "{}-{}-backups", config.project, config.instance_name
        ),
        uniform_bucket_level_access=True,
        public_access_prevention="enforced",
        versioning={"enabled": True},
        lifecycle_rules=[
            {
                "action": {"type": "Delete"},
                "condition": {"age": config.backup_retention_days},
            }
        ],
        opts=pulumi.ResourceOptions(depends_on=apis, protect=True),
    )
    vm_backup_writer = gcp.storage.BucketIAMMember(
        "loom-vm-backup-writer",
        bucket=backup_bucket.name,
        role="roles/storage.objectCreator",
        member=vm_account.email.apply(lambda email: f"serviceAccount:{email}"),
    )

    workload_pool = gcp.iam.WorkloadIdentityPool(
        "loom-github",
        project=config.project,
        workload_identity_pool_id="loom-github",
        display_name="loom GitHub Actions",
        opts=api_options,
    )
    workload_provider = gcp.iam.WorkloadIdentityPoolProvider(
        "loom-github-provider",
        project=config.project,
        workload_identity_pool_id=workload_pool.workload_identity_pool_id,
        workload_identity_pool_provider_id="github",
        display_name="loom GitHub repository",
        attribute_mapping={
            "google.subject": "assertion.sub",
            "attribute.repository": "assertion.repository",
            "attribute.ref": "assertion.ref",
        },
        attribute_condition=(
            f"assertion.repository == '{config.github_repository}' && "
            f"assertion.ref == '{config.github_ref}'"
        ),
        oidc={"issuer_uri": "https://token.actions.githubusercontent.com"},
        opts=api_options,
    )
    principal = workload_pool.name.apply(
        lambda name: (
            f"principalSet://iam.googleapis.com/{name}/attribute.repository/"
            f"{config.github_repository}"
        )
    )
    gcp.serviceaccount.IAMMember(
        "loom-ci-workload-identity",
        service_account_id=ci_account.name,
        role="roles/iam.workloadIdentityUser",
        member=principal,
    )

    image = (
        f"{config.region}-docker.pkg.dev/{config.project}/loom/loom:"
        f"{config.image_tag}"
    )
    metadata = {
        "loom-domain": config.domain,
        "repo-url": config.repo_url,
        "git-ref": config.git_ref,
        "image-mode": config.image_mode,
        "ar-image": image,
        "backup-bucket": backup_bucket.name,
    }
    dependencies: list[pulumi.Resource] = [
        web_firewall,
        ssh_firewall,
        data_disk,
        dotenv_secret,
        secret_version,
        vm_secret_reader,
        vm_image_reader,
        backup_bucket,
        vm_backup_writer,
        snapshot_attachment,
    ]
    if dns_record:
        dependencies.append(dns_record)
    instance = gcp.compute.Instance(
        "loom",
        project=config.project,
        zone=config.zone,
        name=config.instance_name,
        machine_type=config.machine_type,
        tags=["loom-web", "loom-ssh"],
        boot_disk={
            "auto_delete": True,
            "initialize_params": {
                "image": "debian-cloud/debian-12",
                "size": config.boot_disk_gb,
                "type": "pd-balanced",
            },
        },
        attached_disks=[
            # GCE preserves separately attached persistent disks when an
            # instance is deleted; the disk resource is protected as well.
            {
                "source": data_disk.id,
                "device_name": "loom-data",
                "mode": "READ_WRITE",
            }
        ],
        network_interfaces=[
            {
                "network": config.network,
                "access_configs": [{"nat_ip": address.address}],
            }
        ],
        metadata=metadata,
        metadata_startup_script=(ROOT / "deploy/gcp/startup-script.sh").read_text(),
        service_account={
            "email": vm_account.email,
            "scopes": ["cloud-platform"],
        },
        allow_stopping_for_update=True,
        opts=pulumi.ResourceOptions(depends_on=dependencies),
    )

    pulumi.export("address", address.address)
    pulumi.export("url", f"https://{config.domain}/")
    pulumi.export("instanceName", instance.name)
    pulumi.export("zone", config.zone)
    pulumi.export("artifactImage", image)
    pulumi.export("backupBucket", backup_bucket.url)
    pulumi.export("githubWorkloadIdentityProvider", workload_provider.name)
    pulumi.export("githubServiceAccount", ci_account.email)
    if not config.dns_managed_zone:
        pulumi.log.warn(
            "dnsManagedZone is unset: create the exported address's A record "
            "before running post-up.py"
        )

    return Infrastructure(
        address=address,
        instance=instance,
        artifact_repository=artifact_repository,
        backup_bucket=backup_bucket,
        workload_identity_provider=workload_provider,
    )
