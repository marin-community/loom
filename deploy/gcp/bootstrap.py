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
failure (or to change a knob) is safe. Re-running against an existing VM
updates its startup-script + placement metadata in place and re-triggers the
startup script (machine type and disks are left alone — delete the VM to
change those).

It also ensures the LOOM_DOTENV secret exists (pushing it via ./secrets.py if
missing) and, after creating or updating the VM, re-triggers the startup script
and streams it to completion so a single run finishes the deploy. Re-run
./secrets.py yourself whenever loom.toml changes to refresh the pushed config.
"""

import shutil
import socket
import subprocess
import sys
import time
import tomllib
import urllib.error
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
        # stdin=DEVNULL: this path never wants input, and inheriting the
        # parent's stdin risks hanging forever if it's a pipe that never
        # closes (e.g. this script itself run non-interactively).
        result = subprocess.run(
            cmd, check=True, capture_output=True, text=True, stdin=subprocess.DEVNULL
        )
        return result.stdout.strip()
    subprocess.run(cmd, check=True)
    return ""


def gcloud_exists(project: str, *args: str) -> bool:
    result = subprocess.run(
        ["gcloud", f"--project={project}", *args],
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
    )
    return result.returncode == 0


DEPLOY_TOML = Path(__file__).resolve().parent / "deploy.toml"

# The resolved knobs worth remembering between runs — the durable infra choices,
# not the per-invocation operational flags (operator_ip is auto-detected, the
# DNS-wait gate is situational).
REMEMBERED = (
    "project",
    "loom_domain",
    "region",
    "zone",
    "machine_type",
    "disk_size",
    "data_disk_size",
    "instance_name",
    "service_account_name",
    "network",
    "repo_url",
    "git_ref",
    "image_mode",
    "ar_image",
)


def load_deploy_toml() -> dict:
    """Remembered defaults from the last successful run, fed to click as
    `default_map` — so precedence is CLI flag > env var > deploy.toml > the
    built-in defaults. A missing file just means no remembered defaults."""
    if not DEPLOY_TOML.exists():
        return {}
    with DEPLOY_TOML.open("rb") as f:
        return tomllib.load(f)


def save_deploy_toml(values: dict) -> None:
    """Write back the knobs this run resolved, so the next `./bootstrap.py`
    re-deploys with the same settings and no flags. Not secret (secrets live in
    Secret Manager via ./secrets.py) but project-specific, hence gitignored."""
    lines = [
        "# Written by bootstrap.py after a successful run — the remembered deploy",
        "# defaults for next time. Edit or delete freely; flags and env vars still win.",
        "",
    ]
    for key in REMEMBERED:
        v = values.get(key)
        if v is None or v == "":
            continue
        if isinstance(v, bool):
            lines.append(f"{key} = {str(v).lower()}")
        elif isinstance(v, int):
            lines.append(f"{key} = {v}")
        else:
            s = str(v).replace("\\", "\\\\").replace('"', '\\"')
            lines.append(f'{key} = "{s}"')
    DEPLOY_TOML.write_text("\n".join(lines) + "\n")


def default_ar_image(project: str, region: str) -> str:
    """The Artifact Registry image `push-image.py` pushes to and
    `--image-mode=pull` reads — derived from project + region (fixed `loom/loom`
    repo/image) so the path never has to be pasted by hand. Keep in sync with
    push-image.py's copy."""
    return f"{region}-docker.pkg.dev/{project}/loom/loom:latest"


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
) -> bool:
    """Create the VM, or update an existing one's startup-script + placement
    metadata in place. Returns True if it was freshly created (its boot-time
    startup run is the one to watch), False if it already existed (the caller
    re-triggers the startup script). Machine type and disks are never changed on
    an existing instance — delete it first for those."""
    script_dir = Path(__file__).resolve().parent
    startup_script = script_dir / "startup-script.sh"

    metadata = f"loom-domain={loom_domain},repo-url={repo_url},git-ref={git_ref},image-mode={image_mode}"
    if ar_image:
        metadata += f",ar-image={ar_image}"

    if gcloud_exists(
        project, "compute", "instances", "describe", instance_name, f"--zone={zone}"
    ):
        log(f"instance {instance_name} exists — updating startup-script + metadata in place")
        gcloud(
            project,
            "compute",
            "instances",
            "add-metadata",
            instance_name,
            f"--zone={zone}",
            f"--metadata={metadata}",
            f"--metadata-from-file=startup-script={startup_script}",
        )
        log("(machine type and disks are left as-is — delete the VM to change those)")
        return False

    log(f"creating instance {instance_name}")
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
        f"--metadata-from-file=startup-script={startup_script}",
    ]
    if data_disk_size > 0:
        args.append(
            f"--disk=name={data_disk_name},device-name={data_disk_device},mode=rw,boot=no"
        )

    gcloud(project, *args)
    return True


LOOM_DOTENV_SECRET = "LOOM_DOTENV"
STARTUP_UNIT = "google-startup-scripts"
STARTUP_BANNER = "loom startup-script:"  # first line every run logs
STARTUP_DONE = "loom startup-script done"
STARTUP_FAIL = ("failed with error", "Failed with result")


def ensure_secrets(project: str) -> None:
    """Make sure the LOOM_DOTENV blob the startup script fetches exists. If it's
    already there, leave it (re-run ./secrets.py to refresh after loom.toml
    changes). If missing, push it now via ./secrets.py so a first run is
    self-contained."""
    if gcloud_exists(project, "secrets", "describe", LOOM_DOTENV_SECRET):
        log(f"secret {LOOM_DOTENV_SECRET} present — leaving as-is (re-run ./secrets.py to refresh)")
        return
    log(f"secret {LOOM_DOTENV_SECRET} missing — pushing it via ./secrets.py")
    if shutil.which("uv") is None:
        die("need `uv` to run ./secrets.py; install uv or run ./secrets.py yourself first")
    secrets_py = Path(__file__).resolve().parent / "secrets.py"
    result = subprocess.run(
        ["uv", "run", "--script", str(secrets_py), "--project", project]
    )
    if result.returncode != 0:
        die(
            "`./secrets.py` failed — it needs `loom` + a populated loom.toml on this "
            "workstation. Fix that and re-run, or push LOOM_DOTENV yourself. See ./README.md."
        )


def ssh_run(
    project: str, zone: str, instance: str, remote_cmd: str, *, timeout: int = 120
) -> subprocess.CompletedProcess:
    """Run one command on the VM over SSH, capturing output. --quiet accepts the
    key-gen / OS-Login prompts non-interactively."""
    return subprocess.run(
        [
            "gcloud",
            f"--project={project}",
            "compute",
            "ssh",
            instance,
            f"--zone={zone}",
            "--quiet",
            f"--command={remote_cmd}",
        ],
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=timeout,
    )


def wait_for_ssh(project: str, zone: str, instance: str, timeout: int = 300) -> None:
    log("waiting for SSH (the first connect propagates keys, ~1 min)")
    waited = 0
    while waited < timeout:
        try:
            if ssh_run(project, zone, instance, "true", timeout=60).returncode == 0:
                log("SSH is up")
                return
        except subprocess.TimeoutExpired:
            pass
        time.sleep(10)
        waited += 10
    die(
        "SSH never became reachable — check the VM is RUNNING and the SSH "
        "firewall rule allows your current IP"
    )


def count_startup_banners(project: str, zone: str, instance: str) -> int:
    """How many times the startup-script has logged its opening banner so far —
    i.e. how many runs are already in the journal. The monitor waits for this to
    grow before judging, so a re-trigger never reads a previous run's result."""
    r = ssh_run(
        project,
        zone,
        instance,
        f"sudo journalctl -u {STARTUP_UNIT} --no-pager -o cat 2>/dev/null "
        f"| grep -c '{STARTUP_BANNER}' || true",
    )
    try:
        return int(r.stdout.strip() or 0)
    except ValueError:
        return 0


def trigger_startup(project: str, zone: str, instance: str) -> None:
    log("re-triggering the startup-script (detached, via systemd)")
    r = ssh_run(
        project,
        zone,
        instance,
        f"sudo systemctl restart --no-block {STARTUP_UNIT}.service",
    )
    if r.returncode != 0:
        die(f"failed to re-trigger startup-script:\n{r.stderr}")


def monitor_startup(
    project: str, zone: str, instance: str, prior_banners: int, timeout: int = 2400
) -> bool:
    """Watch the startup-script journal until the *current* run — the one past
    `prior_banners` opening banners — prints its done marker or fails. Anchoring
    on the banner count (not a raw line offset) means a re-trigger's monitor
    never trips over an earlier run's failure line still sitting in the journal,
    and it waits for the new run to actually start before judging it. Returns
    whether it succeeded."""
    log("watching startup-script (journalctl) — the build path can take many minutes")
    printed = None
    waited = 0
    while waited < timeout:
        r = ssh_run(
            project,
            zone,
            instance,
            f"sudo journalctl -u {STARTUP_UNIT} --no-pager -o cat 2>/dev/null",
        )
        lines = r.stdout.splitlines() if r.stdout else []
        banners = [i for i, line in enumerate(lines) if STARTUP_BANNER in line]
        if len(banners) <= prior_banners:
            # The (re-)triggered run hasn't logged its banner yet — keep waiting
            # so we never read the previous run's outcome.
            time.sleep(15)
            waited += 15
            continue
        boundary = banners[prior_banners]  # first line of the current run
        if printed is None:
            printed = boundary
        for line in lines[printed:]:
            click.echo(f"  {line}", err=True)
        printed = max(printed, len(lines))
        window = "\n".join(lines[boundary:])
        if STARTUP_DONE in window:
            log("startup-script finished ✔")
            return True
        if any(marker in window for marker in STARTUP_FAIL):
            warn("startup-script FAILED — see the lines above")
            return False
        time.sleep(15)
        waited += 15
    warn(f"startup-script did not finish within {timeout}s")
    return False


def wait_for_https(domain: str, timeout: int = 300) -> bool:
    log(f"checking https://{domain}/ is serving (TLS cert issuance can lag a minute)")
    waited = 0
    while waited < timeout:
        try:
            with urllib.request.urlopen(f"https://{domain}/", timeout=10) as resp:
                log(f"https://{domain}/ responded {resp.status}")
                return True
        except urllib.error.HTTPError as e:
            log(f"https://{domain}/ responded {e.code} — the server is up")
            return True
        except (urllib.error.URLError, OSError):
            pass
        time.sleep(15)
        waited += 15
    return False


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
    "--machine-type", envvar="MACHINE_TYPE", default="e2-highmem-4", show_default=True
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
    default=500,
    show_default=True,
    help="Persistent data-disk size, GB, for loom_home/caddy_data (see "
    "../README.md 'Durable state'). Set to 0 to skip — state then lives "
    "on the boot disk. Only applied to a *new* disk; grow an existing one with "
    "`gcloud compute disks resize` + reboot (the VM grows the filesystem on boot).",
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
    help="Image for --image-mode=pull. Defaults to what ./push-image.py pushes "
    "(<region>-docker.pkg.dev/<project>/loom/loom:latest), so it's usually left "
    "blank; set it only to pull some other prebuilt image.",
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
        ar_image = default_ar_image(project, region)
        log(f"--image-mode=pull: no --ar-image given, using {ar_image}")
        log("  (build+push it there first with ./push-image.py)")

    enable_apis(project, image_mode)
    ensure_service_account(project, sa_email, service_account_name, image_mode)
    ensure_firewall(project, network, fw_web, fw_ssh, operator_ip)
    ip = ensure_static_ip(project, region, ip_name)
    wait_for_dns(loom_domain, ip, dns_wait_seconds, skip_dns_wait)
    ensure_data_disk(project, zone, data_disk_name, data_disk_size)
    ensure_secrets(project)
    created = ensure_instance(
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

    save_deploy_toml(
        {
            "project": project,
            "loom_domain": loom_domain,
            "region": region,
            "zone": zone,
            "machine_type": machine_type,
            "disk_size": disk_size,
            "data_disk_size": data_disk_size,
            "instance_name": instance_name,
            "service_account_name": service_account_name,
            "network": network,
            "repo_url": repo_url,
            "git_ref": git_ref,
            "image_mode": image_mode,
            "ar_image": ar_image,
        }
    )

    log(f"remembered these settings in {DEPLOY_TOML.name} — a bare re-run reuses them")

    # Drive the deploy to completion: apply the (possibly updated) startup
    # script and watch it land, rather than leaving the operator to tail logs.
    wait_for_ssh(project, zone, instance_name)
    if created:
        prior_banners = 0  # fresh VM: its boot-time run is the first
    else:
        # Existing VM: count the runs already in the journal, then re-trigger and
        # watch only for the new one, so a prior failure isn't mistaken for it.
        prior_banners = count_startup_banners(project, zone, instance_name)
        trigger_startup(project, zone, instance_name)

    ok = monitor_startup(project, zone, instance_name, prior_banners)
    click.echo("", err=True)
    if not ok:
        die(
            "deploy did not finish — inspect the VM:\n"
            f"  gcloud --project={project} compute ssh {instance_name} --zone={zone} "
            f"--command='sudo journalctl -u {STARTUP_UNIT} --no-pager | tail -60'"
        )

    log(f"startup-script succeeded. VM: {instance_name}  IP: {ip}  domain: {loom_domain}")
    if wait_for_https(loom_domain):
        log(f"loom is live: https://{loom_domain}/")
    else:
        warn(
            f"stack is up but https://{loom_domain}/ isn't answering yet — the TLS "
            "certificate may still be issuing. Watch it:\n"
            f"  gcloud --project={project} compute ssh {instance_name} --zone={zone} "
            "--command='cd /opt/loom/deploy/standalone && sudo docker compose logs -f caddy'"
        )
    log(
        "manual-once (if not done): first OAuth login + install the App on your "
        "repos — see ./README.md."
    )


if __name__ == "__main__":
    # deploy.toml → click default_map: remembered defaults that a flag or env var
    # still overrides. See load_deploy_toml.
    main(default_map=load_deploy_toml())
