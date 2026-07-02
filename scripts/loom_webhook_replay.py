#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["click", "httpx"]
# ///
"""Replay a GitHub `issue_comment` webhook at a loom instance.

Short-circuits the whole GitHub round-trip: no PR comment, no redeploy. Signs the
body with the same HMAC the receiver checks and mints a FRESH delivery GUID each
run, so loom's dedupe (`processed_deliveries`) never drops it. See
docs/github-trigger.md → "Debugging the trigger".

    # against a local dev loom
    export LOOM_GITHUB_WEBHOOK_SECRET=dev-secret
    ./loom_webhook_replay.py --url http://127.0.0.1:8080 \
        --repo owner/name --author your-login --body '@loom rebase onto main'

    # or replay a payload captured from GitHub's "Recent Deliveries" tab
    ./loom_webhook_replay.py --url http://127.0.0.1:8080 --payload delivery.json
"""
import hashlib
import hmac
import json
import uuid

import click
import httpx


def sign(secret: str, body: bytes) -> str:
    return "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def synth_payload(repo, author, body, number, title):
    owner, _, name = repo.partition("/")
    return {
        "action": "created",
        "issue": {"number": number, "title": title, "body": ""},
        "comment": {"body": body, "user": {"login": author}},
        "repository": {"full_name": repo, "name": name, "owner": {"login": owner}},
    }


@click.command()
@click.option("--url", required=True, help="loom base URL, e.g. http://127.0.0.1:8080")
@click.option("--secret", envvar="LOOM_GITHUB_WEBHOOK_SECRET",
              help="webhook HMAC secret (or env LOOM_GITHUB_WEBHOOK_SECRET)")
@click.option("--payload", type=click.File("rb"), default=None,
              help="raw payload JSON captured from GitHub (overrides the synth options)")
@click.option("--repo", default="owner/name", help="owner/name slug")
@click.option("--author", default="octocat", help="commenter login")
@click.option("--body", default="@loom", help="comment body")
@click.option("--number", default=1, type=int, help="issue/PR number")
@click.option("--title", default="test issue")
@click.option("--event", default="issue_comment")
def main(url, secret, payload, repo, author, body, number, title, event):
    if not secret:
        raise click.UsageError("no secret: pass --secret or set LOOM_GITHUB_WEBHOOK_SECRET")
    raw = payload.read() if payload else json.dumps(
        synth_payload(repo, author, body, number, title)).encode()
    delivery = str(uuid.uuid4())  # fresh every run -> bypasses dedupe
    headers = {
        "content-type": "application/json",
        "x-github-event": event,
        "x-github-delivery": delivery,
        "x-hub-signature-256": sign(secret, raw),
    }
    endpoint = url.rstrip("/") + "/api/github/webhook"
    click.echo(f"POST {endpoint}  delivery={delivery}")
    click.echo(f"  event={event} repo={repo} author={author} body={body!r}")
    r = httpx.post(endpoint, content=raw, headers=headers, timeout=30)
    click.echo(f"-> HTTP {r.status_code}  {r.text!r}")
    if r.status_code == 401:
        click.echo("  401 = signature/secret mismatch (secret != server's, or unset)")
    elif r.status_code == 200 and r.text.strip('"') == "ok":
        click.echo("  200 ok = accepted; check loom logs to see which gate it hit / if it launched")


if __name__ == "__main__":
    main()
