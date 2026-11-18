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

Rendering needs a `loom` binary, but never a host-side build: the script
builds the compose image — where loom is compiled anyway — and renders .env
from a one-shot container of it.

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
import tempfile
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


def docker_gid() -> str | None:
    """The host's `docker` group gid, so the loom container's app user can join
    it (compose `group_add`) and reach the bind-mounted Docker socket for
    `docker build`. None when there's no such group (e.g. Docker Desktop, which
    runs its own Linux VM unrelated to any macOS/Windows group) — the caller
    then passes DOCKER_GID=0 explicitly, opting into Dockerfile's
    loom-docker-socket-init chmod fallback rather than leaving DOCKER_GID unset
    (compose requires it; see docker-compose.yml)."""
    try:
        import grp

        return str(grp.getgrnam("docker").gr_gid)
    except (ImportError, KeyError):
        return None


def config_path(config: str | None) -> Path:
    """The loom.toml this run renders: the flag, then $LOOM_CONFIG, else the
    repo-root default (`--config`'s help text)."""
    return Path(config or os.environ.get("LOOM_CONFIG") or REPO_ROOT / "loom.toml").resolve()


def env_value(name: str) -> str | None:
    """One KEY=VALUE from the rendered .env, if it is set there."""
    if not ENV_PATH.exists():
        return None
    for line in ENV_PATH.read_text().splitlines():
        key, _, value = line.partition("=")
        if key.strip() == name:
            return value.strip()
    return None


def interpolated(name: str, fallback: str) -> str:
    """A compose interpolation variable at compose's own precedence: the
    process env, then what a previous render left in .env, else the fallback
    the compose file declares."""
    return os.environ.get(name) or env_value(name) or fallback


def image_tag() -> str:
    """The image tag this run builds and deploys (${LOOM_IMAGE:-loom:latest})."""
    return interpolated("LOOM_IMAGE", "loom:latest")


def image_exists(tag: str) -> bool:
    probe = subprocess.run(
        ["docker", "image", "inspect", tag],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return probe.returncode == 0


def write_env(rendered: str) -> None:
    """Write the rendered dotenv 0600 — it can hold secrets, matching the
    permissions `loom config render-env` itself writes with."""
    ENV_PATH.write_text(rendered)
    os.chmod(ENV_PATH, 0o600)


def render_env(config: str | None, local_: bool, no_build: bool, env: dict) -> None:
    """Render .env from the compose image: build it (the only place loom needs
    to be built), then run `loom config render-env` in a one-shot container
    of it.

    Raw `docker run` rather than `docker compose run`: every compose service
    lists .env as its env_file, so a compose-spawned render would inherit the
    very file it is re-rendering — and render-env resolves a same-named env
    var over the file, so those stale values would win over loom.toml."""
    cfg = config_path(config)
    if not cfg.exists() and not ENV_PATH.exists():
        die(
            f"neither {cfg} nor {ENV_PATH} exists — copy loom.toml.example to "
            "loom.toml and fill it in (or run `loom setup`) first."
        )
    tag = image_tag()
    if no_build:
        if not image_exists(tag):
            die(f"no {tag} image to render from — drop --no-build for a first build")
        log(f"rendering .env from the existing {tag} image")
    else:
        log(f"building the {tag} image to render .env from")
        # Plain `docker build`: compose cannot even load docker-compose.yml
        # before .env exists — it is every service's env_file, and LOOM_DOMAIN
        # is required just to interpolate the file — and this build only
        # exists to give the render an image. `up --build` is the
        # authoritative build, re-run with the rendered .env.
        build = ["docker", "build", "-t", tag, str(REPO_ROOT)]
        # The build args docker-compose.yml declares, at compose's own
        # precedence and defaults, so `up --build` cache-hits this image.
        for arg, fallback in (
            ("HOST_UID", "1000"),
            ("HOST_GID", "1000"),
            ("CARGO_PROFILE", "debug"),
        ):
            build += ["--build-arg", f"{arg}={interpolated(arg, fallback)}"]
        run(build, env=env)

    # Root so the mount reads whatever uid owns loom.toml on the host (the
    # image's app user may differ); render-env only reads and prints.
    cmd = ["docker", "run", "--rm", "--user", "0:0", "-v", f"{cfg.parent}:/loom-cfg:ro"]
    # render-env resolves a same-named env var over loom.toml — the way an
    # operator overrides a single value for one run without editing the file —
    # so forward the ambient environment into the render to keep that working.
    # One generated env-file rather than -e flags per variable: the variable
    # names are loom's own contract, not this script's to duplicate.
    # PATH/HOME/LOOM_CONFIG stay out (the image's own PATH must resolve the
    # entrypoint, and --config below names the mounted copy), as do values
    # with newlines, which env-file's line format can't carry.
    forwarded = {
        key: value
        for key, value in os.environ.items()
        if "\n" not in value and key not in ("PATH", "HOME", "HOSTNAME", "LOOM_CONFIG")
    }
    with tempfile.NamedTemporaryFile("w", prefix="loom-render-env.") as env_file:
        env_file.write("".join(f"{key}={value}\n" for key, value in forwarded.items()))
        env_file.flush()
        cmd += ["--env-file", env_file.name]
        if local_:
            cmd += ["-e", "LOOM_DOMAIN=localhost"]
        cmd += [
            tag,
            "loom",
            "config",
            "render-env",
            "--config",
            f"/loom-cfg/{cfg.name}",
            "--out",
            "-",
        ]
        log(" ".join(cmd))
        result = subprocess.run(cmd, env=env, capture_output=True, text=True)
    if result.stderr:
        click.echo(result.stderr, err=True)
    if result.returncode != 0:
        die(f"rendering .env from the {tag} image failed")
    if not result.stdout.strip():
        die(f"rendering .env from the {tag} image produced nothing — is {cfg} filled in?")
    write_env(result.stdout)


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
    help="Skip the image rebuild (`up` without `--build`; the image must "
    "already exist, since .env is rendered from it).",
)
def main(local_: bool, config: str | None, no_build: bool) -> None:
    """Render .env and bring up the standalone loom stack."""
    if not shutil.which("docker"):
        die("`docker` not found — install Docker (with the compose plugin) first.")

    env = os.environ.copy()
    # BuildKit is required for the Dockerfile's cargo cache mounts — the default
    # with compose v2, but set explicitly so an older/opted-out Docker still gets
    # the fast, cached build. (CARGO_PROFILE defaults to debug in the compose
    # file; export CARGO_PROFILE=release before running for a production build.)
    # The buildx builder is whatever the operator's docker defaults to (the
    # docker driver of the current context); an exported BUILDX_BUILDER passes
    # through untouched.
    env["DOCKER_BUILDKIT"] = "1"
    # Pass the host docker gid through for the loom service's `group_add`, so
    # sessions can reach the bind-mounted Docker socket (see docker-compose.yml).
    env.setdefault("DOCKER_GID", docker_gid() or "0")
    if local_:
        env["LOOM_DOMAIN"] = "localhost"

    # 1. Render deploy/standalone/.env from loom.toml via the image.
    render_env(config, local_, no_build, env)

    # 2. Refuse early if no operator is configured — the daemon would otherwise
    #    boot-loop on its "no operator" guard.
    if not env_value("LOOM_OWNER_GITHUB"):
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
