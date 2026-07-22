#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click>=8.1"]
# ///
"""Bring the standalone Loom Compose stack up with one command.

It does the two manual steps for you: render deploy/standalone/.env from your
loom.toml, then `docker compose up -d --build` here. With --local it targets
LOOM_DOMAIN=localhost, so Caddy signs with its own internal CA (self-signed —
the browser warns, expected) and you can exercise the real three-service stack
(Caddy + loom-init + loom) on your own machine before pointing DNS at a server.

Why .env lives next to the compose file (a common question): docker compose
auto-loads `.env` from the compose project directory and uses it both to
interpolate the compose file (${LOOM_DOMAIN}, ${LOOM_IMAGE}) and as each
service's `env_file:`. It has to sit here, beside docker-compose.yml — this
script writes it there so you don't have to think about the path.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

import click

HERE = Path(__file__).resolve().parent  # deploy/standalone
REPO_ROOT = HERE.parent.parent
ENV_PATH = HERE / ".env"


def log(msg: str) -> None:
    click.echo(f"▶ {msg}", err=True)


def warn(msg: str) -> None:
    click.echo(f"⚠ {msg}", err=True)


def die(msg: str) -> None:
    click.echo(f"✘ {msg}", err=True)
    sys.exit(1)


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    log(" ".join(cmd))
    return subprocess.run(cmd, check=True, **kw)


def find_loom() -> str:
    """The `loom` binary: PATH first, then a local cargo build under target/."""
    on_path = shutil.which("loom")
    if on_path:
        return on_path
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / "loom"
        if candidate.is_file():
            return str(candidate)
    die("`loom` not found — build it (`cargo build -p loom`) or put it on PATH.")


def docker_gid() -> str | None:
    """The host's `docker` group gid, so the loom container's app user can join
    it (compose `group_add`) and reach the bind-mounted Docker socket for
    `docker build`. None when there's no such group (e.g. Docker Desktop on
    macOS, where the socket isn't group-owned anyway) — the compose file's 999
    fallback then applies."""
    try:
        import grp

        return str(grp.getgrnam("docker").gr_gid)
    except (ImportError, KeyError):
        return None


def owner_configured(env_text: str) -> bool:
    """Whether the rendered env names a non-empty LOOM_OWNER_GITHUB. Without one
    the daemon refuses to boot (it would come up locked out), so we fail early
    with a pointer to `loom setup` rather than after `compose up`."""
    for line in env_text.splitlines():
        key, _, value = line.partition("=")
        if key.strip() == "LOOM_OWNER_GITHUB":
            return bool(value.strip())
    return False


@click.command(context_settings={"help_option_names": ["-h", "--help"]})
@click.option(
    "--local",
    "local_",
    is_flag=True,
    help="Target localhost (Caddy uses its internal CA; the browser warns).",
)
@click.option(
    "--config",
    default=None,
    help="loom.toml path (default: repo-root loom.toml or $LOOM_CONFIG).",
)
@click.option(
    "--no-build",
    is_flag=True,
    help="Skip the image rebuild (`up` without `--build`).",
)
def main(local_: bool, config: str | None, no_build: bool) -> None:
    """Render .env and bring up the standalone loom stack."""
    if not shutil.which("docker"):
        die("`docker` not found — install Docker (with the compose plugin) first.")
    loom = find_loom()

    # 1. Render deploy/standalone/.env from loom.toml. For --local, override the
    #    domain via the process env — loom_config resolves an env var over the
    #    file, so this doesn't touch loom.toml itself.
    render = [loom, "config", "render-env", "--out", str(ENV_PATH)]
    if config:
        render += ["--config", config]
    env = os.environ.copy()
    # BuildKit is required for the Dockerfile's cargo cache mounts — the default
    # with compose v2, but set explicitly so an older/opted-out Docker still gets
    # the fast, cached build. (CARGO_PROFILE defaults to debug in the compose
    # file; export CARGO_PROFILE=release before running for a production build.)
    env["DOCKER_BUILDKIT"] = "1"
    # Build with the `docker` driver (writes straight into the image store) rather
    # than a `docker-container` builder, which would export the large image as an
    # OCI tarball and re-load it — tens of seconds per build. This local, single-
    # arch deploy doesn't need a multi-arch container builder. Respects an
    # explicit BUILDX_BUILDER if you've set one.
    env.setdefault("BUILDX_BUILDER", "default")
    # Pass the host docker gid through for the loom service's `group_add`, so
    # sessions can reach the bind-mounted Docker socket (see docker-compose.yml).
    gid = docker_gid()
    if gid:
        env.setdefault("DOCKER_GID", gid)
    if local_:
        env["LOOM_DOMAIN"] = "localhost"
    run(render, env=env)

    # 2. Refuse early if no operator is configured — the daemon would otherwise
    #    boot-loop on its "no operator" guard.
    if not owner_configured(ENV_PATH.read_text()):
        die(
            "no operator in the rendered env (LOOM_OWNER_GITHUB is empty) — run "
            "`loom setup` (or set LOOM_OWNER_GITHUB in loom.toml) first."
        )

    # 3. Print where to go and how to log in BEFORE starting: `docker compose up`
    #    runs in the foreground (streams logs, Ctrl+C to stop), so anything
    #    printed after it would only appear once the stack was already stopped.
    url = "https://localhost" if local_ else "https://<LOOM_DOMAIN>"
    click.echo()
    log("starting the stack in the foreground — Ctrl+C to stop it")
    click.echo(f"  once it's up, open   {url}")
    if local_:
        click.echo("  note   self-signed cert (Caddy internal CA) — your browser will warn")
        # Behind Caddy the request never looks like loopback, so trust_loopback
        # can't let you in — sign-in is GitHub OAuth, and a local stack needs the
        # App's callback list to include the localhost origin.
        has_oauth = any(
            line.startswith("LOOM_GITHUB_CLIENT_ID=") and line.partition("=")[2].strip()
            for line in ENV_PATH.read_text().splitlines()
        )
        if has_oauth:
            click.echo(
                "  sign-in  the GitHub App/OAuth callback list must include\n"
                "           https://localhost/api/auth/github/callback"
            )
        else:
            click.echo(
                "  sign-in  not configured yet — run\n"
                "           loom setup github-app --base-url https://localhost"
            )
    click.echo()

    # 4. Build + run the three services attached (loom-init seeds auth settings,
    #    then loom, then caddy). Run from HERE so compose finds docker-compose.yml
    #    and the .env we just wrote; blocks until Ctrl+C.
    up = ["docker", "compose", "up"]
    if not no_build:
        up.append("--build")
    run(up, cwd=str(HERE), env=env)


if __name__ == "__main__":
    main()
