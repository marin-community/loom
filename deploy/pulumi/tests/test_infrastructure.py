from __future__ import annotations

import inspect
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
        if args.typ == "gcp:storage/bucket:Bucket":
            outputs["url"] = f"gs://{args.name}"
        return f"{args.name}_id", outputs

    def call(self, args: MockCallArgs):
        return args.args


mocks = RecordingMocks()
pulumi.runtime.set_mocks(mocks, project="loom-gcp", stack="test", preview=False)

from infrastructure import (  # noqa: E402
    DeploymentConfig,
    _positive_config_int,
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
    assert set(re.findall(r'role="([^"]+)"', source)) == {
        "roles/iam.workloadIdentityUser",
        "roles/artifactregistry.writer",
        "roles/artifactregistry.reader",
        "roles/secretmanager.secretAccessor",
        "roles/storage.objectCreator",
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
