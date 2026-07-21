#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click>=8.1"]
# ///
"""Verify and activate a loom VM after ``pulumi up``.

Pulumi owns durable resources. This small imperative tail deliberately owns the
checks that are observations rather than resources: public DNS propagation,
SSH readiness, the current startup-script journal run, and HTTPS health.
"""

from __future__ import annotations

import json
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import click


PULUMI_DIR = Path(__file__).resolve().parent
STARTUP_UNIT = "google-startup-scripts"
STARTUP_BANNER = "loom startup-script:"
STARTUP_DONE = "loom startup-script done"
STARTUP_FAILURES = ("failed with error", "Failed with result")


def log(message: str) -> None:
    click.echo(f"▶ {message}", err=True)


def fail(message: str) -> None:
    raise click.ClickException(message)


def stack_outputs(stack: str | None) -> dict[str, str]:
    command = ["pulumi", "stack", "output", "--json", "--cwd", str(PULUMI_DIR)]
    if stack:
        command.extend(["--stack", stack])
    result = subprocess.run(
        command,
        check=True,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
    )
    return json.loads(result.stdout)


def resolved_ipv4(domain: str) -> set[str]:
    try:
        return {
            item[4][0]
            for item in socket.getaddrinfo(domain, 443, socket.AF_INET)
        }
    except OSError:
        return set()


def wait_for_dns(domain: str, address: str, timeout: int) -> None:
    log(f"waiting for {domain} to resolve publicly to {address}")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if address in resolved_ipv4(domain):
            return
        time.sleep(15)
    fail(
        f"{domain} does not resolve to {address}; delegate the configured Cloud "
        "DNS zone (or create the A record) before activating loom"
    )


def ssh(
    project: str, zone: str, instance: str, command: str, timeout: int = 120
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "gcloud",
            f"--project={project}",
            "compute",
            "ssh",
            instance,
            f"--zone={zone}",
            "--quiet",
            f"--command={command}",
        ],
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=timeout,
    )


def wait_for_ssh(project: str, zone: str, instance: str, timeout: int) -> None:
    log("waiting for SSH")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if ssh(project, zone, instance, "true", timeout=60).returncode == 0:
                return
        except subprocess.TimeoutExpired:
            pass
        time.sleep(10)
    fail("SSH never became reachable; check the operator CIDR firewall rule")


def journal(project: str, zone: str, instance: str) -> list[str]:
    result = ssh(
        project,
        zone,
        instance,
        f"sudo journalctl -u {STARTUP_UNIT} --no-pager -o cat 2>/dev/null",
    )
    return result.stdout.splitlines()


def activate_and_monitor(
    project: str, zone: str, instance: str, timeout: int
) -> None:
    # Anchor monitoring to a newly-triggered generation. Old failure text in the
    # persistent journal must never make this deployment look failed.
    prior = sum(STARTUP_BANNER in line for line in journal(project, zone, instance))
    log("triggering the current startup script")
    result = ssh(
        project,
        zone,
        instance,
        f"sudo systemctl restart --no-block {STARTUP_UNIT}.service",
    )
    if result.returncode:
        fail(f"could not trigger startup script: {result.stderr.strip()}")

    log("streaming the current startup-script generation")
    deadline = time.monotonic() + timeout
    printed = 0
    while time.monotonic() < deadline:
        lines = journal(project, zone, instance)
        banners = [index for index, line in enumerate(lines) if STARTUP_BANNER in line]
        if len(banners) <= prior:
            time.sleep(15)
            continue
        boundary = banners[prior]
        start = max(boundary, printed)
        for line in lines[start:]:
            click.echo(f"  {line}", err=True)
        printed = len(lines)
        window = "\n".join(lines[boundary:])
        if STARTUP_DONE in window:
            return
        if any(marker in window for marker in STARTUP_FAILURES):
            fail("startup script failed; inspect the journal output above")
        time.sleep(15)
    fail("startup script did not finish before the timeout")


def health_is_ready(domain: str) -> bool:
    """Return whether loom's exact public liveness contract is healthy once."""
    try:
        with urllib.request.urlopen(
            f"https://{domain}/api/health", timeout=10
        ) as response:
            return response.status == 200 and response.read().strip() == b"ok"
    except (urllib.error.HTTPError, urllib.error.URLError, OSError):
        # Caddy may already answer while its loom upstream is still starting;
        # 401/404/5xx are not success for this exact public endpoint.
        return False


def wait_for_health(domain: str, timeout: int) -> None:
    log(f"checking https://{domain}/api/health")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if health_is_ready(domain):
            log("loom health check responded 200 ok")
            return
        time.sleep(15)
    fail("the startup script succeeded, but loom's HTTPS health check is not ready")


@click.command(context_settings={"help_option_names": ["-h", "--help"]})
@click.option("--stack", help="Pulumi stack name. Default: current stack.")
@click.option("--dns-timeout", default=1200, show_default=True)
@click.option("--ssh-timeout", default=300, show_default=True)
@click.option("--startup-timeout", default=2400, show_default=True)
@click.option("--https-timeout", default=300, show_default=True)
def main(
    stack: str | None,
    dns_timeout: int,
    ssh_timeout: int,
    startup_timeout: int,
    https_timeout: int,
) -> None:
    outputs = stack_outputs(stack)
    try:
        project = subprocess.run(
            [
                "pulumi",
                "config",
                "get",
                "gcp:project",
                "--cwd",
                str(PULUMI_DIR),
                *(["--stack", stack] if stack else []),
            ],
            check=True,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        ).stdout.strip()
        address = outputs["address"]
        instance = outputs["instanceName"]
        zone = outputs["zone"]
        domain = outputs["url"].removeprefix("https://").rstrip("/")
    except (KeyError, subprocess.CalledProcessError) as error:
        fail(f"could not read the Pulumi stack outputs: {error}")

    wait_for_dns(domain, address, dns_timeout)
    wait_for_ssh(project, zone, instance, ssh_timeout)
    activate_and_monitor(project, zone, instance, startup_timeout)
    wait_for_health(domain, https_timeout)
    log(f"loom is live: https://{domain}/")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        click.echo(error.stderr or str(error), err=True)
        sys.exit(error.returncode)
