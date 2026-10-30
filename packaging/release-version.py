#!/usr/bin/env python3
"""Resolve a published version for manual, tag, or workflow-dispatch releases."""
import json
import os
import urllib.request

from versioning import parse_release


def api(path):
    request = urllib.request.Request(
        f"https://api.github.com/repos/{os.environ['GITHUB_REPOSITORY']}/{path}",
        headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}",
                 "Accept": "application/vnd.github+json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def release_job_succeeded(run_id):
    """A dry run succeeds with its `release` job skipped: nothing was published."""
    jobs = api(f"actions/runs/{run_id}/jobs?per_page=100")["jobs"]
    return any(job["name"] == "release" and job["conclusion"] == "success" for job in jobs)


def main():
    version = os.environ.get("REQUESTED_VERSION", "")
    run_id = os.environ.get("RELEASE_RUN_ID", "")
    if not version and run_id and not release_job_succeeded(run_id):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write("publish=false\n")
        print("Skipping: the triggering release workflow did not publish a release")
        return
    if not version:
        branch = os.environ.get("RELEASE_BRANCH", "")
        if branch.startswith("v") and parse_release(branch[1:]):
            version = branch
        else:
            sha = os.environ.get("RELEASE_SHA", "")
            releases = api("releases?per_page=100")
            matches = [r for r in releases if sha and r["target_commitish"] == sha
                       and not r["draft"]]
            if len(matches) != 1:
                raise SystemExit("Cannot identify one published release for the triggering commit")
            version = matches[0]["tag_name"]
    version = version.removeprefix("v")
    match = parse_release(version)
    if not match:
        raise SystemExit("Expected a version such as 1.7.0 or 2.0.0-alpha.1")
    channel = match[0]
    release = api(f"releases/tags/v{version}")
    if release["draft"]:
        raise SystemExit("Package channels cannot publish draft releases")
    if release["prerelease"] != (channel != "stable"):
        raise SystemExit("Release prerelease flag does not match its version")
    publish = channel == "stable" or os.environ.get("ALLOW_PRERELEASE") == "true"
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"version={version}\nchannel={channel}\npublish={str(publish).lower()}\n")
    if publish:
        print(f"Updating {channel} GUI and CLI packages to {version}")
    else:
        print(f"Skipping prerelease {version}: this package channel only publishes stable releases")


if __name__ == "__main__":
    main()
