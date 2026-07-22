from __future__ import annotations

import inspect
import json
import re

import pulumi
import pytest
from pulumi.runtime import MockCallArgs, MockResourceArgs, Mocks


class RecordingMocks(Mocks):
    def __init__(self) -> None:
        self.resources: list[MockResourceArgs] = []

    def new_resource(self, args: MockResourceArgs):
        self.resources.append(args)
        outputs = dict(args.inputs)
        outputs.setdefault("name", args.name)
        if args.typ == "gcp:compute/address:Address":
            outputs["address"] = "203.0.113.10"
        if args.typ == "gcp:serviceaccount/account:Account":
            outputs["email"] = f"{args.name}@example.iam.gserviceaccount.com"
            outputs["uniqueId"] = "11223344556677889900"
            outputs["unique_id"] = "11223344556677889900"
        if args.typ == "gcp:storage/bucket:Bucket":
            outputs["url"] = f"gs://{args.name}"
        return f"{args.name}_id", outputs

    def call(self, args: MockCallArgs):
        return args.args


mocks = RecordingMocks()
pulumi.runtime.set_mocks(mocks, project="loom-gcp", stack="test", preview=False)

from infrastructure import (  # noqa: E402
    DeploymentConfig,
    WorkloadIdentityConfig,
    _google_federation_mapping,
    _positive_config_int,
    _profile_manifest,
    create_infrastructure,
)


def make_infrastructure():
    mocks.resources.clear()
    return create_infrastructure(
        DeploymentConfig(
            project="example",
            region="us-central1",
            zone="us-central1-a",
            domain="loom.example.com",
            operator_cidr="203.0.113.7/32",
            loom_dotenv=pulumi.Output.secret("LOOM_DOMAIN=loom.example.com\n"),
            dns_managed_zone="example-zone",
            image_mode="pull",
            image_tag="0123456789abcdef0123456789abcdef01234567",
            profiles={
                "ops": {
                    "agent": "codex",
                    "protocol": "acp",
                    "mode": "plan",
                    "class": "automation",
                    "strict": True,
                    "envClear": True,
                    "maxConcurrent": 1,
                    "env": {
                        "KUBECONFIG": {
                            "secretRef": "projects/example/secrets/ops-kubeconfig/versions/latest"
                        }
                    },
                }
            },
            workloads=(
                WorkloadIdentityConfig.parse(
                    {
                        "name": "marin-ops",
                        "profile": "ops",
                        "serviceTag": "marin-ops",
                        "serviceAccountId": "loom-marin-ops",
                    }
                ),
            ),
        )
    )


def by_name(name: str) -> MockResourceArgs:
    return next(resource for resource in mocks.resources if resource.name == name)


def field(inputs: dict, snake: str, camel: str):
    return inputs.get(snake, inputs.get(camel))


def test_pull_mode_requires_a_commit_sha() -> None:
    with pytest.raises(ValueError, match="commit-SHA"):
        DeploymentConfig(
            project="example",
            region="us-central1",
            zone="us-central1-a",
            domain="loom.example.com",
            operator_cidr="203.0.113.7/32",
            loom_dotenv="LOOM_DOMAIN=loom.example.com\n",
            image_mode="pull",
            image_tag="latest",
        )


def test_profile_manifest_accepts_references_but_not_secret_values() -> None:
    profiles, references = _profile_manifest(
        {
            "ops": {
                "agent": "codex",
                "strict": True,
                "envClear": True,
                "env": {
                    "OPS_TOKEN": {
                        "secretRef": "projects/example/secrets/ops-token/versions/7"
                    }
                },
            }
        }
    )
    assert profiles[0]["env"] == [
        {
            "name": "OPS_TOKEN",
            "secret_ref": "projects/example/secrets/ops-token/versions/7",
        }
    ]
    assert profiles[0]["profile"]["prelude"] == "weaver"
    assert profiles[0]["profile"]["restricted"] is False
    assert profiles[0]["profile"]["allowed_tools"] == []
    assert references == [("example", "ops-token")]
    with pytest.raises(ValueError, match="full secretRef"):
        _profile_manifest(
            {"ops": {"agent": "codex", "env": {"OPS_TOKEN": "plaintext"}}}
        )


def test_profile_manifest_round_trips_restricted_policy() -> None:
    profiles, _ = _profile_manifest(
        {
            "github_comment": {
                "agent": "claude",
                "prelude": "none",
                "restricted": True,
                "allowedTools": [
                    "Read(./**)",
                    "mcp__loom_github__issue_view",
                ],
            }
        }
    )
    profile = profiles[0]["profile"]
    assert profile["prelude"] == "none"
    assert profile["restricted"] is True
    assert profile["allowed_tools"] == [
        "Read(./**)",
        "mcp__loom_github__issue_view",
    ]


def test_google_mapping_binds_numeric_subject_email_and_profile() -> None:
    workload = WorkloadIdentityConfig.parse(
        {
            "name": "marin-ops",
            "profile": "ops",
            "serviceTag": "marin-ops",
            "serviceAccountId": "loom-marin-ops",
        }
    )
    mapping = _google_federation_mapping(
        workload,
        "https://loom.example.com",
        "loom-marin-ops@example.iam.gserviceaccount.com",
        "11223344556677889900",
    )
    assert mapping["subject"] == "11223344556677889900"
    assert mapping["service_account"].endswith(".iam.gserviceaccount.com")
    assert mapping["profiles"] == ["ops"]


@pytest.mark.parametrize("account_id", ["short", "a" * 31, "Upper-case"])
def test_workload_service_account_ids_follow_gcp_limits(account_id: str) -> None:
    with pytest.raises(ValueError, match="serviceAccountId"):
        WorkloadIdentityConfig.parse(
            {
                "name": "marin-ops",
                "profile": "ops",
                "serviceAccountId": account_id,
            }
        )


@pytest.mark.parametrize(
    ("value", "default", "expected"),
    [(None, 14, 14), (7, 14, 7)],
)
def test_positive_config_int_defaults_only_absence(
    value: int | None, default: int, expected: int
) -> None:
    assert _positive_config_int(value, default, "retentionDays") == expected


@pytest.mark.parametrize("value", [0, -1])
@pytest.mark.parametrize(
    "name", ["bootDiskGb", "dataDiskGb", "snapshotRetentionDays", "backupRetentionDays"]
)
def test_positive_config_int_rejects_explicit_nonpositive(value: int, name: str) -> None:
    with pytest.raises(ValueError, match=f"{name} must be positive"):
        _positive_config_int(value, 10, name)


@pulumi.runtime.test
def test_vm_keeps_data_disk_and_installs_backup_metadata():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        vm = by_name("loom")
        attached = field(vm.inputs, "attached_disks", "attachedDisks")
        assert len(attached) == 1
        assert field(attached[0], "device_name", "deviceName") == "loom-data"
        # An attached persistent disk is not auto-deleted with a GCE instance;
        # the source Disk also has a Pulumi protect option in the source below.
        assert field(attached[0], "auto_delete", "autoDelete") is not True
        metadata = vm.inputs["metadata"]
        assert field(metadata, "backup-bucket", "backup-bucket")
        assert metadata["image-mode"] == "pull"

    return infrastructure.instance.id.apply(check)


@pulumi.runtime.test
def test_wif_is_repository_and_main_bound_with_least_privilege_iam():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        provider = by_name("loom-github-provider")
        condition = field(
            provider.inputs, "attribute_condition", "attributeCondition"
        )
        assert "rjpower/weaver" in condition
        assert "refs/heads/main" in condition

        repository = by_name("loom-images")
        docker_config = field(repository.inputs, "docker_config", "dockerConfig")
        assert field(docker_config, "immutable_tags", "immutableTags") is True

    return infrastructure.workload_identity_provider.id.apply(check)


def test_iam_roles_are_narrow() -> None:
    source = inspect.getsource(create_infrastructure)
    assert set(re.findall(r'"(roles/[^"]+)"', source)) == {
        "roles/iam.workloadIdentityUser",
        "roles/artifactregistry.writer",
        "roles/artifactregistry.reader",
        "roles/secretmanager.secretAccessor",
        "roles/storage.objectCreator",
        "roles/monitoring.metricWriter",
        "roles/logging.logWriter",
    }


@pulumi.runtime.test
def test_backup_bucket_enforces_public_access_prevention():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        bucket = by_name("loom-backups")
        prevention = field(
            bucket.inputs, "public_access_prevention", "publicAccessPrevention"
        )
        assert prevention == "enforced"

    return infrastructure.instance.id.apply(check)


@pulumi.runtime.test
def test_profiles_workload_identity_and_monitoring_are_declarative():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        names = [resource.name for resource in mocks.resources]
        assert "loom-workload-marin-ops" in names
        assert "loom-profile-secret-" in " ".join(names)
        assert "loom-readiness" in names
        assert "loom-operations" in names
        assert "loom-readiness-failed" in names

        vm = by_name("loom")
        manifest = json.loads(vm.inputs["metadata"]["loom-deployment"])
        assert manifest["prune"] is True
        assert manifest["profiles"][0]["profile"]["name"] == "ops"
        env = manifest["profiles"][0]["env"][0]
        assert env == {
            "name": "KUBECONFIG",
            "secret_ref": "projects/example/secrets/ops-kubeconfig/versions/latest",
        }
        mapping = manifest["federations"][0]
        assert mapping["provider"] == "google"
        assert mapping["subject"] == "11223344556677889900"
        assert mapping["service_account"].endswith(".iam.gserviceaccount.com")
        assert mapping["profiles"] == ["ops"]

    return infrastructure.dashboard.id.apply(check)


@pulumi.runtime.test
def test_secret_version_is_bound_to_managed_secret():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        secret = by_name("loom-dotenv")
        version = by_name("loom-dotenv-current")
        managed_secret = field(version.inputs, "secret", "secret")
        assert managed_secret == f"{secret.name}_id"
        assert field(version.inputs, "secret_data", "secretData")

    return infrastructure.instance.id.apply(check)


@pulumi.runtime.test
def test_vm_waits_for_seeded_secret_and_all_runtime_grants():
    infrastructure = make_infrastructure()

    def check(_: object) -> None:
        names = [resource.name for resource in mocks.resources]
        vm_index = names.index("loom")
        for prerequisite in (
            "loom-dotenv-current",
            "loom-vm-secret-reader",
            "loom-vm-image-reader",
            "loom-vm-backup-writer",
            "loom-data-snapshot-policy",
        ):
            assert names.index(prerequisite) < vm_index

        source = inspect.getsource(create_infrastructure)
        dependencies = source[source.index("dependencies:") : source.index("instance =")]
        for variable in (
            "secret_version",
            "vm_secret_reader",
            "vm_image_reader",
            "vm_backup_writer",
            "snapshot_attachment",
        ):
            assert variable in dependencies

    return infrastructure.instance.id.apply(check)


def test_stateful_resources_are_source_protected():
    source = inspect.getsource(create_infrastructure)
    for declaration in (
        '"loom-address"',
        '"loom-data"',
        '"loom-dotenv"',
        '"loom-backups"',
    ):
        start = source.index(declaration)
        assert "protect=True" in source[start : start + 700]
    version = source[source.index('"loom-dotenv-current"') :]
    assert "retain_on_delete=True" in version[:500]
