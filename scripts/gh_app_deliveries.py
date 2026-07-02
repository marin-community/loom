#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyjwt[crypto]", "httpx", "click"]
# ///
"""List the loom GitHub App's recent webhook deliveries — GitHub's "Recent
Deliveries" tab, from the command line.

Mints an App JWT from the App id + private key in loom.toml, then reads the App's
installations and its recent webhook deliveries (including, for `issue_comment`
events, the repo, comment, and the HTTP status loom replied with). See
docs/github-trigger.md → "Debugging the trigger".

    ./gh_app_deliveries.py --config /path/to/loom.toml
"""
import time
import tomllib

import click
import httpx
import jwt


def app_jwt(app_id: str, private_key: str) -> str:
    now = int(time.time())
    return jwt.encode({"iat": now - 60, "exp": now + 540, "iss": app_id},
                      private_key, algorithm="RS256")


@click.command()
@click.option("--config", default="loom.toml", envvar="LOOM_CONFIG",
              type=click.Path(exists=True, dir_okay=False),
              help="path to loom.toml (or env LOOM_CONFIG)")
@click.option("--limit", default=50, type=int, help="how many deliveries to list")
def main(config, limit):
    cfg = tomllib.load(open(config, "rb"))
    try:
        app_id = str(cfg["github_app_id"])
        key = cfg["github_app_private_key"]
    except KeyError as e:
        raise click.UsageError(f"{config} is missing {e} — is the GitHub App configured?")

    h = {"Authorization": f"Bearer {app_jwt(app_id, key)}",
         "Accept": "application/vnd.github+json",
         "X-GitHub-Api-Version": "2022-11-28"}

    app = httpx.get("https://api.github.com/app", headers=h).json()
    click.echo(f"App: {app.get('slug')!r} (id={app.get('id')})  {app.get('html_url')}")

    insts = httpx.get("https://api.github.com/app/installations", headers=h).json()
    click.echo(f"\nInstallations ({len(insts)}):")
    for i in insts:
        acct = i.get("account", {})
        click.echo(f"  - {acct.get('login')} ({acct.get('type')})  "
                   f"repos={i.get('repository_selection')}  id={i.get('id')}")

    r = httpx.get(f"https://api.github.com/app/hook/deliveries?per_page={limit}", headers=h)
    if r.status_code != 200:
        raise click.ClickException(f"deliveries HTTP {r.status_code}: {r.text[:300]}")
    ds = r.json()
    click.echo(f"\nRecent deliveries ({len(ds)}):")
    click.echo(f"  {'delivered_at':25} {'event':16} {'action':9} {'status':6} redel")
    for d in ds:
        click.echo(f"  {d.get('delivered_at',''):25} {str(d.get('event')):16} "
                   f"{str(d.get('action')):9} {str(d.get('status_code')):6} {d.get('redelivery')}")

    click.echo("\n--- issue_comment delivery details ---")
    for d in ds:
        if d.get("event") != "issue_comment":
            continue
        det = httpx.get(f"https://api.github.com/app/hook/deliveries/{d['id']}", headers=h).json()
        payload = det.get("request", {}).get("payload", {}) or {}
        comment = (payload.get("comment", {}) or {}).get("body", "")
        author = (payload.get("comment", {}) or {}).get("user", {}).get("login")
        resp = det.get("response", {}) or {}
        click.echo(f"  [{d.get('delivered_at')}] repo={payload.get('repository', {}).get('full_name')} "
                   f"action={payload.get('action')} author={author}")
        click.echo(f"      comment={comment[:80]!r}")
        click.echo(f"      loom response: HTTP {resp.get('status_code')}  "
                   f"body={str(resp.get('payload'))[:120]!r}")


if __name__ == "__main__":
    main()
