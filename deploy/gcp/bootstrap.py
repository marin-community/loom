#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click>=8.1"]
# ///
"""Provisions a single GCE VM that runs the standalone loom stack
(../standalone/docker-compose.yml) behind its bundled Caddy front-door. Run
this from your workstation, not the VM. See ./README.md for the full runbook
and the required run order.

This is Model 1 (single host) only — see ../README.md "Future: cloud /
cluster". It does not provision a cluster, Cloud Run, or per-session
isolation.

Every gcloud call here is check-before-create, so re-running after a partial
failure (or to change a knob) is safe.

Secrets (GH_TOKEN, ANTHROPIC_API_KEY, ...) are NOT handled here — run
./secrets.py first (or after; order doesn't matter, see ../README.md).
"""

import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import click


def log(msg: str) -> None:
    click.echo(f"▶ {msg}", err=True)


def warn(msg: str) -> None:
    click.echo(f"⚠ {msg}", err=True)


def die(msg: str) -> None:
    click.echo(msg, err=True)
    sys.exit(1)


def gcloud(project: str, *args: str, capture: bool = False) -> str:
    cmd = ["gcloud", f"--project={project}", *args]
    if capture:
        result = subprocess.run(cmd, check=True, capture_output=True, text=True)
        return result.stdout.strip()
    subprocess.run(cmd, check=True)
    return ""


def gcloud_exists(project: str, *args: str) -> bool:
    result = subprocess.run(
        ["gcloud", f"--project={project}", *args],
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def enable_apis(project: str, image_mode: str) -> None:
    log("enabling required APIs")
    apis = ["compute.googleapis.com", "secretmanager.googleapis.com"]
    if image_mode == "pull":
        apis.append("artifactregistry.googleapis.com")
    gcloud(project, "services", "enable", *apis)


def ensure_service_account(
    project: str, sa_email: str, sa_name: str, image_mode: str
) -> None:
    log(f"ensuring service account {sa_email}")
    if not gcloud_exists(project, "iam", "service-accounts", "describe", sa_email):
        gcloud(
            project,
            "iam",
            "service-accounts",
            "create",
            sa_name,
            "--display-name=loom standalone VM",
        )
    # Least-privilege: only Secret Manager read access. The startup script uses
    # this identity (via the metadata server's token endpoint, through gcloud)
    # to fetch the secrets it writes into deploy/standalone/.env.
    gcloud(
        project,
        "projects",
        "add-iam-policy-binding",
        project,
        f"--member=serviceAccount:{sa_email}",
        "--role=roles/secretmanager.secretAccessor",
        "--condition=None",
    )
    if image_mode == "pull":
        gcloud(
            project,
            "projects",
            "add-iam-policy-binding",
            project,
            f"--member=serviceAccount:{sa_email}",
            "--role=roles/artifactregistry.reader",
            "--condition=None",
        )


def detect_operator_ip() -> str:
    log("auto-detecting operator public IP")
    try:
        with urllib.request.urlopen("https://api.ipify.org", timeout=10) as resp:
            return resp.read().decode().strip()
    except OSError:
        return ""


def ensure_firewall(
    project: str, network: str, fw_web: str, fw_ssh: str, operator_ip: str
) -> None:
    if not operator_ip:
        operator_ip = detect_operator_ip()
    if not operator_ip:
        die("could not auto-detect your public IP; set --operator-ip explicitly")

    log(f"ensuring firewall rule {fw_web} (tcp:80,tcp:443,udp:443 from 0.0.0.0/0)")
    if not gcloud_exists(project, "compute", "firewall-rules", "describe", fw_web):
        gcloud(
            project,
            "compute",
            "firewall-rules",
            "create",
            fw_web,
            f"--network={network}",
            "--direction=INGRESS",
            "--action=ALLOW",
            "--rules=tcp:80,tcp:443,udp:443",
            "--source-ranges=0.0.0.0/0",
            "--target-tags=loom-web",
        )

    log(f"ensuring firewall rule {fw_ssh} (tcp:22 from {operator_ip}/32 only)")
    if not gcloud_exists(project, "compute", "firewall-rules", "describe", fw_ssh):
        gcloud(
            project,
            "compute",
            "firewall-rules",
            "create",
            fw_ssh,
            f"--network={network}",
            "--direction=INGRESS",
            "--action=ALLOW",
            "--rules=tcp:22",
            f"--source-ranges={operator_ip}/32",
            "--target-tags=loom-ssh",
        )
    else:
        gcloud(
            project,
            "compute",
            "firewall-rules",
            "update",
            fw_ssh,
            f"--source-ranges={operator_ip}/32",
        )
    # 7878 (loom's own port) is intentionally never opened here — the only way
    # in is through Caddy on 80/443. See ../standalone/docker-compose.yml.


def ensure_static_ip(project: str, region: str, ip_name: str) -> str:
    log(f"ensuring static external IP {ip_name}")
    if not gcloud_exists(
        project, "compute", "addresses", "describe", ip_name, f"--region={region}"
    ):
        gcloud(project, "compute", "addresses", "create", ip_name, f"--region={region}")
    return gcloud(
        project,
        "compute",
        "addresses",
        "describe",
        ip_name,
        f"--region={region}",
        "--format=value(address)",
        capture=True,
    )


def resolve(domain: str) -> str:
    try:
        return socket.gethostbyname(domain)
    except OSError:
        return ""


def wait_for_dns(domain: str, ip: str, wait_seconds: int, skip_wait: bool) -> None:
    click.echo("", err=True)
    click.echo("═" * 70, err=True)
    click.echo(
        "  Set this DNS record before continuing (ACME HTTP-01 needs it to", err=True
    )
    click.echo(
        "  resolve BEFORE the stack starts, or the TLS certificate won't issue):",
        err=True,
    )
    click.echo("", err=True)
    click.echo(f"    {domain}.   A   {ip}", err=True)
    click.echo("", err=True)
    click.echo("═" * 70, err=True)

    if skip_wait:
        warn("--skip-dns-wait — not waiting for DNS to resolve")
        return

    log(f"waiting up to {wait_seconds}s for {domain} to resolve to {ip}")
    waited = 0
    while waited < wait_seconds:
        if resolve(domain) == ip:
            log(f"{domain} resolves to {ip}")
            return
        time.sleep(15)
        waited += 15

    warn(f"{domain} does not resolve to {ip} yet after {wait_seconds}s")
    if not click.confirm("Continue and create the VM anyway?", default=False):
        die("aborting; re-run bootstrap.py once DNS is set")


def ensure_data_disk(project: str, zone: str, disk_name: str, size_gb: int) -> None:
    if size_gb <= 0:
        return
    log(f"ensuring data disk {disk_name} ({size_gb}GB)")
    if not gcloud_exists(
        project, "compute", "disks", "describe", disk_name, f"--zone={zone}"
    ):
        gcloud(
            project,
            "compute",
            "disks",
            "create",
            disk_name,
            f"--zone={zone}",
            f"--size={size_gb}GB",
            "--type=pd-balanced",
        )


def ensure_instance(
    project: str,
    zone: str,
    instance_name: str,
    machine_type: str,
    disk_size: int,
    sa_email: str,
    ip_name: str,
    loom_domain: str,
    repo_url: str,
    git_ref: str,
    image_mode: str,
    ar_image: str,
    data_disk_size: int,
    data_disk_name: str,
    data_disk_device: str,
) -> None:
    log(f"ensuring instance {instance_name}")
    if gcloud_exists(
        project, "compute", "instances", "describe", instance_name, f"--zone={zone}"
    ):
        log(f"instance {instance_name} already exists — leaving it as-is")
        log(
            "(delete it first if you want bootstrap.py to recreate it with new settings)"
        )
        return

    script_dir = Path(__file__).resolve().parent

    metadata = f"loom-domain={loom_domain},repo-url={repo_url},git-ref={git_ref},image-mode={image_mode}"
    if ar_image:
        metadata += f",ar-image={ar_image}"

    args = [
        "compute",
        "instances",
        "create",
        instance_name,
        f"--zone={zone}",
        f"--machine-type={machine_type}",
        "--image-family=debian-12",
        "--image-project=debian-cloud",
        f"--boot-disk-size={disk_size}GB",
        "--boot-disk-type=pd-balanced",
        "--tags=loom-web,loom-ssh",
        f"--service-account={sa_email}",
        "--scopes=cloud-platform",
        f"--address={ip_name}",
        f"--metadata={metadata}",
        f"--metadata-from-file=startup-script={script_dir / 'startup-script.sh'}",
    ]
    if data_disk_size > 0:
        args.append(
            f"--disk=name={data_disk_name},device-name={data_disk_device},mode=rw,boot=no"
        )

    gcloud(project, *args)


@click.command(context_settings={"help_option_names": ["-h", "--help"]})
@click.option("--project", envvar="PROJECT", required=True, help="GCP project id.")
@click.option(
    "--loom-domain",
    envvar="LOOM_DOMAIN",
    required=True,
    help="Public domain the VM will serve.",
)
@click.option("--region", envvar="REGION", default="us-central1", show_default=True)
@click.option("--zone", envvar="ZONE", default=None, help="Default: <region>-a")
@click.option(
    "--machine-type", envvar="MACHINE_TYPE", default="e2-standard-4", show_default=True
)
@click.option(
    "--disk-size",
    envvar="DISK_SIZE",
    default=100,
    show_default=True,
    help="Boot disk size, GB.",
)
@click.option(
    "--data-disk-size",
    envvar="DATA_DISK_SIZE",
    default=50,
    show_default=True,
    help="Persistent data-disk size, GB, for loom_home/caddy_data (see "
    "../README.md 'Durable state'). Set to 0 to skip — state then lives "
    "on the boot disk.",
)
@click.option(
    "--instance-name", envvar="INSTANCE_NAME", default="loom", show_default=True
)
@click.option(
    "--service-account-name",
    envvar="SERVICE_ACCOUNT_NAME",
    default="loom-vm",
    show_default=True,
    help="A dedicated, low-privilege SA (granted only secretmanager.secretAccessor).",
)
@click.option("--network", envvar="NETWORK", default="default", show_default=True)
@click.option(
    "--operator-ip",
    envvar="OPERATOR_IP",
    default="",
    help="Used for the SSH firewall rule's /32. Default: auto-detected public IP. "
    "Override if you SSH from a different network than you run this script from.",
)
@click.option(
    "--repo-url",
    envvar="REPO_URL",
    default="https://github.com/rjpower/weaver.git",
    show_default=True,
    help="Git URL the VM clones to get deploy/standalone.",
)
@click.option("--git-ref", envvar="GIT_REF", default="main", show_default=True)
@click.option(
    "--image-mode",
    envvar="IMAGE_MODE",
    type=click.Choice(["build", "pull"]),
    default="build",
    show_default=True,
    help="'build': VM builds the image itself with `docker compose up -d --build` "
    "(slow, needs the roomy default machine). 'pull': VM pulls a prebuilt "
    "image from Artifact Registry (--ar-image required); see "
    "../README.md 'Build once, pull many'.",
)
@click.option(
    "--ar-image",
    envvar="AR_IMAGE",
    default="",
    help="Required when --image-mode=pull, e.g. "
    "us-central1-docker.pkg.dev/$PROJECT/loom/loom:latest",
)
@click.option(
    "--dns-wait-seconds",
    envvar="DNS_WAIT_SECONDS",
    default=600,
    show_default=True,
    help="How long to poll for the DNS record before asking whether to proceed anyway.",
)
@click.option(
    "--skip-dns-wait",
    envvar="SKIP_DNS_WAIT",
    is_flag=True,
    help="Skip the DNS wait/confirmation gate entirely (e.g. re-running against "
    "an already-live domain).",
)
def main(
    project: str,
    loom_domain: str,
    region: str,
    zone: str | None,
    machine_type: str,
    disk_size: int,
    data_disk_size: int,
    instance_name: str,
    service_account_name: str,
    network: str,
    operator_ip: str,
    repo_url: str,
    git_ref: str,
    image_mode: str,
    ar_image: str,
    dns_wait_seconds: int,
    skip_dns_wait: bool,
) -> None:
    zone = zone or f"{region}-a"
    sa_email = f"{service_account_name}@{project}.iam.gserviceaccount.com"
    ip_name = f"{instance_name}-ip"
    data_disk_name = f"{instance_name}-data"
    data_disk_device = "loom-data"
    fw_web = f"{instance_name}-allow-web"
    fw_ssh = f"{instance_name}-allow-ssh"

    if image_mode == "pull" and not ar_image:
        die(
            f"--image-mode=pull requires --ar-image (e.g. "
            f"us-central1-docker.pkg.dev/{project}/loom/loom:latest)"
        )

    enable_apis(project, image_mode)
    ensure_service_account(project, sa_email, service_account_name, image_mode)
    ensure_firewall(project, network, fw_web, fw_ssh, operator_ip)
    ip = ensure_static_ip(project, region, ip_name)
    wait_for_dns(loom_domain, ip, dns_wait_seconds, skip_dns_wait)
    ensure_data_disk(project, zone, data_disk_name, data_disk_size)
    ensure_instance(
        project,
        zone,
        instance_name,
        machine_type,
        disk_size,
        sa_email,
        ip_name,
        loom_domain,
        repo_url,
        git_ref,
        image_mode,
        ar_image,
        data_disk_size,
        data_disk_name,
        data_disk_device,
    )

    click.echo("", err=True)
    log(f"done. VM: {instance_name}  IP: {ip}  domain: {loom_domain}")
    log("next: run ./secrets.py if you haven't yet, then watch the boot:")
    log(f"  gcloud --project={project} compute ssh {instance_name} --zone={zone} \\")
    log("    --command='sudo journalctl -u google-startup-scripts -f'")
    log(
        "see ./README.md for the manual-once checklist (OAuth app, first login, webhook)."
    )


if __name__ == "__main__":
    main()
