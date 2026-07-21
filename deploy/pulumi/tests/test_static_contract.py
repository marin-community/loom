from __future__ import annotations

import ast
import re
import unittest
from pathlib import Path


PULUMI_DIR = Path(__file__).resolve().parents[1]
ROOT = PULUMI_DIR.parents[1]
SOURCE = (PULUMI_DIR / "infrastructure.py").read_text()


class InfrastructureSourceContract(unittest.TestCase):
    def test_required_resource_types_are_declared(self) -> None:
        tree = ast.parse(SOURCE)
        calls = {
            ast.unparse(node.func)
            for node in ast.walk(tree)
            if isinstance(node, ast.Call)
        }
        self.assertTrue(
            {
                "gcp.compute.Instance",
                "gcp.compute.Disk",
                "gcp.compute.ResourcePolicy",
                "gcp.compute.DiskResourcePolicyAttachment",
                "gcp.compute.Address",
                "gcp.compute.Firewall",
                "gcp.dns.RecordSet",
                "gcp.secretmanager.Secret",
                "gcp.secretmanager.SecretVersion",
                "gcp.storage.Bucket",
                "gcp.artifactregistry.Repository",
                "gcp.iam.WorkloadIdentityPool",
                "gcp.iam.WorkloadIdentityPoolProvider",
            }.issubset(calls)
        )

    def test_vm_dependency_block_contains_runtime_prerequisites(self) -> None:
        dependencies = SOURCE[
            SOURCE.index("dependencies:") : SOURCE.index("instance =")
        ]
        for name in (
            "secret_version",
            "vm_secret_reader",
            "vm_image_reader",
            "vm_backup_writer",
            "snapshot_attachment",
        ):
            self.assertIn(name, dependencies)

    def test_backup_uses_sqlite_online_api_and_integrity_check(self) -> None:
        backup = (ROOT / "deploy/gcp/backup-sqlite.sh").read_text()
        self.assertIn(".backup", backup)
        self.assertIn("PRAGMA quick_check", backup)
        self.assertIn("gcloud storage cp", backup)

    def test_image_workflow_is_keyless_and_immutable(self) -> None:
        workflow = (ROOT / ".github/workflows/image.yml").read_text()
        self.assertIn("id-token: write", workflow)
        self.assertIn("google-github-actions/auth@", workflow)
        self.assertIn("${GITHUB_SHA}", workflow)
        self.assertIn("gcloud artifacts docker images describe", workflow)
        self.assertIn("steps.image.outputs.exists != 'true'", workflow)
        self.assertNotIn("loom:latest", workflow)
        self.assertNotIn("credentials_json", workflow)
        self.assertIn('docker_config={"immutable_tags": True}', SOURCE)
        action_refs = re.findall(r"uses:\s+[^@\s]+@([^\s#]+)", workflow)
        self.assertTrue(action_refs)
        self.assertTrue(all(re.fullmatch(r"[0-9a-f]{40}", ref) for ref in action_refs))


if __name__ == "__main__":
    unittest.main()
