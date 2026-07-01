#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click>=8.1"]
# ///
"""Pushes loom's config to Secret Manager for the GCP deploy to fetch on boot.
Run from your workstation, where the `loom` binary lives — the VM only ever
gets the container image (startup-script.sh doesn't invoke `loom` at all; see
its header). Run any time before or after bootstrap.py, and again whenever
loom.toml changes.

This script never hardcodes a config field name — the shared `loom config`
commands (crates/loom's `LoomConfig` schema) own that. Two modes:

  Blob (default): `loom config render-env` renders the whole dotenv (every
  field, secret or not — see `loom config --help`) and this script stores it
  as ONE Secret Manager secret, LOOM_DOTENV. startup-script.sh fetches that
  one secret and writes it straight to deploy/standalone/.env — the VM never
  knows an individual field name either.

  --granular: delegate to `loom config push-secrets`, which pushes each
  secret field to its own Secret Manager secret (id == its ENV_NAME), for
  independent rotation. startup-script.sh as shipped expects the blob; pair
  this mode with a startup-script.sh that fetches fields individually if you
  use it.

Usage:
  PROJECT=my-project ./secrets.py                    # blob mode
  PROJECT=my-project ./secrets.py --config loom.toml
  PROJECT=my-project ./secrets.py --granular
"""

import shutil
import subprocess
import sys
from pathlib import Path

import click

LOOM_DOTENV_SECRET = "LOOM_DOTENV"


def die(msg: str) -> None:
    click.echo(msg, err=True)
    sys.exit(1)


def require_loom() -> None:
    if shutil.which("loom") is None:
        die(
            "`loom` not found on PATH — this script runs on your workstation, "
            "where the loom binary lives (the VM only ever gets the "
            "container image; see startup-script.sh). Build/install it first."
        )


def run(cmd: list[str], *, input_bytes: bytes | None = None) -> bytes:
    # stdin=DEVNULL when there's no input to send: subprocess otherwise
    # inherits the parent's stdin, which can hang forever if it's a pipe that
    # never closes (e.g. this script itself run non-interactively).
    result = subprocess.run(
        cmd,
        input=input_bytes,
        capture_output=True,
        stdin=subprocess.DEVNULL if input_bytes is None else None,
    )
    if result.returncode != 0:
        die(f"$ {' '.join(cmd)}\n{result.stderr.decode(errors='replace')}")
    return result.stdout


def gcloud_exists(project: str, *args: str) -> bool:
    result = subprocess.run(
        ["gcloud", f"--project={project}", *args],
        capture_output=True,
        stdin=subprocess.DEVNULL,
    )
    return result.returncode == 0


@click.command(context_settings={"help_option_names": ["-h", "--help"]})
@click.option("--project", envvar="PROJECT", required=True, help="GCP project id.")
@click.option(
    "--config",
    "config_path",
    envvar="LOOM_CONFIG",
    default=None,
    type=click.Path(path_type=Path, exists=True, dir_okay=False),
    help="Path to loom.toml. Default: loom's own default config path "
    "(see `loom config render-env --help`).",
)
@click.option(
    "--granular",
    is_flag=True,
    help="Push each secret field to its own Secret Manager secret via "
    "`loom config push-secrets`, instead of one LOOM_DOTENV blob. "
    "startup-script.sh as shipped expects the blob mode.",
)
def main(project: str, config_path: Path | None, granular: bool) -> None:
    require_loom()
    config_args = ["--config", str(config_path)] if config_path else []

    if granular:
        run(
            [
                "loom",
                "config",
                "push-secrets",
                "--backend",
                "gcp",
                "--project",
                project,
                *config_args,
            ]
        )
        click.echo(
            "▶ pushed per-field secrets via `loom config push-secrets`", err=True
        )
        return

    rendered = run(["loom", "config", "render-env", "--out", "-", *config_args])

    if not gcloud_exists(project, "secrets", "describe", LOOM_DOTENV_SECRET):
        click.echo(f"▶ creating secret {LOOM_DOTENV_SECRET}", err=True)
        run(
            [
                "gcloud",
                f"--project={project}",
                "secrets",
                "create",
                LOOM_DOTENV_SECRET,
                "--replication-policy=automatic",
            ]
        )

    run(
        [
            "gcloud",
            f"--project={project}",
            "secrets",
            "versions",
            "add",
            LOOM_DOTENV_SECRET,
            "--data-file=-",
        ],
        input_bytes=rendered,
    )
    click.echo(f"▶ set {LOOM_DOTENV_SECRET}", err=True)


if __name__ == "__main__":
    main()
