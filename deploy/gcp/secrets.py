#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click>=8.1"]
# ///
"""Creates/updates the Secret Manager secrets startup-script.sh reads on boot.
Run from your workstation, any time before or after bootstrap.py (the VM's
service account can read Secret Manager regardless of instance state) — and
again whenever you need to rotate a value.

Usage:
  PROJECT=my-project ./secrets.py                  # prompts for every secret
  PROJECT=my-project ./secrets.py GH_TOKEN          # just one
  PROJECT=my-project GH_TOKEN=ghp_xxx ./secrets.py  # non-interactive: reads
                                                     # from an already-exported
                                                     # env var of the same name

Values are never echoed and never appear as a process argument: interactive
entry uses a hidden prompt, and both paths pipe the value to gcloud over stdin.
"""

import os
import subprocess
import sys

import click

SECRET_NAMES = [
    "GH_TOKEN",
    "ANTHROPIC_API_KEY",
    "LOOM_GITHUB_WEBHOOK_SECRET",
    "LOOM_GITHUB_CLIENT_ID",
    "LOOM_GITHUB_CLIENT_SECRET",
    "LOOM_OWNER_GITHUB",
    "LOOM_DOMAIN",
]


def gcloud_exists(project: str, *args: str) -> bool:
    result = subprocess.run(
        ["gcloud", f"--project={project}", *args],
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def gcloud(project: str, *args: str, input_text: str | None = None) -> None:
    subprocess.run(
        ["gcloud", f"--project={project}", *args],
        check=True,
        input=input_text,
        text=True,
        capture_output=True,
    )


@click.command(
    context_settings={"help_option_names": ["-h", "--help"]},
    help="Create/update secrets. NAMES defaults to all of: " + ", ".join(SECRET_NAMES),
)
@click.option("--project", envvar="PROJECT", required=True, help="GCP project id.")
@click.argument("names", nargs=-1)
def main(project: str, names: tuple[str, ...]) -> None:
    selected = list(names) or SECRET_NAMES

    for name in selected:
        if name not in SECRET_NAMES:
            click.echo(
                f"unknown secret name: {name} (expected one of: {', '.join(SECRET_NAMES)})",
                err=True,
            )
            sys.exit(1)

    for name in selected:
        value = os.environ.get(name, "")
        if not value:
            value = click.prompt(
                f"value for {name}", hide_input=True, default="", show_default=False
            )
        if not value:
            click.echo(f"empty value for {name}, skipping", err=True)
            continue

        if not gcloud_exists(project, "secrets", "describe", name):
            click.echo(f"▶ creating secret {name}", err=True)
            gcloud(project, "secrets", "create", name, "--replication-policy=automatic")
        gcloud(
            project,
            "secrets",
            "versions",
            "add",
            name,
            "--data-file=-",
            input_text=value,
        )
        click.echo(f"▶ set {name}", err=True)
        del value


if __name__ == "__main__":
    main()
