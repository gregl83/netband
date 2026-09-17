#!/usr/bin/env python3
"""Prepare a draft release, or verify its downloaded assets before publication."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")
ASSETS = tuple(f"netband-{target}.tar.gz{suffix}" for target in TARGETS
               for suffix in ("", ".sha256")) + (
                   "netband-installer.sh", "netband.toml", "netband.service")
DRAFT_NOTES = "Release assets prepared by CD. Review the successful preparation run and add release notes before publishing."


def gh(*args, missing_ok=False):
    result = subprocess.run(["gh", *args], capture_output=True, text=True)
    if result.returncode:
        if missing_ok and "(HTTP 404)" in result.stderr:
            return None
        raise RuntimeError(result.stderr.strip())
    return result.stdout


def check_assets(directory):
    for name in ASSETS:
        if not (directory / name).is_file():
            raise ValueError(f"Missing release asset: {name}")


def verify(directory, sha):
    check_assets(directory)
    for target in TARGETS:
        archive = f"netband-{target}.tar.gz"
        digest = hashlib.sha256((directory / archive).read_bytes()).hexdigest()
        if (directory / f"{archive}.sha256").read_text().split() != [digest, archive]:
            raise ValueError(f"Invalid checksum: {archive}")
    repo = os.environ["GH_REPO"]
    for name in ASSETS:
        gh("attestation", "verify", str(directory / name), "--repo", repo,
           "--source-digest", sha, "--signer-workflow", f"{repo}/.github/workflows/cd.yml")


def prepare(tag, sha, directory):
    check_assets(directory)
    reference = gh("api", f"repos/{{owner}}/{{repo}}/git/ref/tags/{tag}", missing_ok=True)
    if reference:
        obj = json.loads(reference)["object"]
        while obj["type"] == "tag":
            obj = json.loads(gh("api", f"repos/{{owner}}/{{repo}}/git/tags/{obj['sha']}"))["object"]
        if obj["type"] != "commit" or obj["sha"] != sha:
            raise ValueError(f"Tag {tag} points to another commit; refusing to move it")
    pages = json.loads(gh("api", "repos/{owner}/{repo}/releases", "--paginate", "--slurp"))
    existing = next((release for page in pages for release in page if release["tag_name"] == tag), None)
    if existing and not existing["draft"]:
        raise ValueError(f"Release {tag} is already published; refusing to modify it")
    verify(directory, sha)
    if not reference:
        gh("api", "repos/{owner}/{repo}/git/refs", "--method", "POST",
           "-f", f"ref=refs/tags/{tag}", "-f", f"sha={sha}")
    if not existing:
        gh("release", "create", tag, "--verify-tag", "--draft", "--title", tag,
           "--notes", DRAFT_NOTES)
    gh("release", "upload", tag, *(str(directory / name) for name in ASSETS), "--clobber")
    with tempfile.TemporaryDirectory() as downloaded:
        gh("release", "download", tag, "--dir", downloaded)
        verify(Path(downloaded), sha)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "verify"))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--assets", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", args.tag):
        parser.error("tag must be a version such as v1.0.0")
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        parser.error("sha must be a full commit SHA")
    if args.command == "prepare":
        prepare(args.tag, args.sha, args.assets)
    else:
        verify(args.assets, args.sha)
