#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Prepares an AV Relay release: one version for the sender and the receiver.

    scripts/release.py 0.2.0            # version everywhere, changelog, commit, tag v0.2.0
    git push origin main v0.2.0         # the Release workflow builds and publishes it

    scripts/release.py --check 0.2.0    # CI: does the tree say 0.2.0 everywhere?
    scripts/release.py --notes 0.2.0    # CI: the release notes (the changelog's section)

Preparing moves CHANGELOG.md's "Unreleased" entries under the new version,
sets the version in the Linux package (Cargo.toml, Cargo.lock, the AppStream
metainfo), commits, and makes the annotated tag. The Mac app takes its
version from the tag when it's built. Pushing the tag starts the release.
Versions follow Semantic Versioning; one with a "-" (0.2.0-rc.1) is
published as a pre-release. Details: docs/releasing.md.
"""

import argparse
import datetime
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPO_URL = "https://github.com/salihmarangoz/av_relay"
CHANGELOG = ROOT / "CHANGELOG.md"
CARGO = ROOT / "client/linux/Cargo.toml"
LOCK = ROOT / "client/linux/Cargo.lock"
METAINFO = ROOT / "client/linux/packaging/io.github.salihmarangoz.avrelay.metainfo.xml"
VERSION = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$")
SECTION = re.compile(r"^## \[([^\]]+)\](?: - (\d{4}-\d{2}-\d{2}))?[ \t]*$", re.M)


def fail(message):
    print(f"release: {message}", file=sys.stderr)
    sys.exit(1)


# ---------- reading ----------------------------------------------------------


def sections(text):
    """The changelog's version sections: [(name, date, start, body_start, end)]."""
    found = list(SECTION.finditer(text))
    links = re.search(r"^\[[^\]]+\]: ", text, re.M)
    stop = links.start() if links else len(text)
    out = []
    for i, m in enumerate(found):
        end = found[i + 1].start() if i + 1 < len(found) else stop
        out.append((m.group(1), m.group(2), m.start(), m.end(), end))
    return out


def notes(version):
    """The changelog's entries for `version`."""
    text = CHANGELOG.read_text()
    for name, _, _, body, end in sections(text):
        if name == version:
            return text[body:end].strip()
    return None


def cargo_version():
    m = re.search(r'^version = "([^"]+)"', CARGO.read_text(), re.M)
    return m.group(1) if m else None


def lock_version():
    m = re.search(r'^\[\[package\]\]\nname = "av-relay"\nversion = "([^"]+)"', LOCK.read_text(), re.M)
    return m.group(1) if m else None


def metainfo_version():
    m = re.search(r'<releases>\s*<release version="([^"]+)"', METAINFO.read_text())
    return m.group(1) if m else None


def problems(version):
    """What doesn't say `version` yet (empty: ready to release)."""
    found = []
    if cargo_version() != version:
        found.append(f"client/linux/Cargo.toml says {cargo_version()}")
    if lock_version() != version:
        found.append(f"client/linux/Cargo.lock says {lock_version()} for av-relay")
    if metainfo_version() != version:
        found.append(f"the metainfo's newest release is {metainfo_version()}")
    entries = notes(version)
    if entries is None:
        found.append(f"CHANGELOG.md has no section for {version}")
    elif not entries:
        found.append(f"CHANGELOG.md's section for {version} is empty")
    return found


# ---------- writing ----------------------------------------------------------


def set_changelog(version, date):
    text = CHANGELOG.read_text()
    secs = sections(text)
    if not secs or secs[0][0] != "Unreleased":
        fail("CHANGELOG.md must start with an '## [Unreleased]' section")
    _, _, start, body, end = secs[0]
    if not text[body:end].strip():
        fail("nothing under '## [Unreleased]' in CHANGELOG.md: write down what changed first")
    if any(s[0] == version for s in secs):
        fail(f"CHANGELOG.md already has a section for {version}")
    previous = next((s[0] for s in secs[1:]), None)
    # "## [Unreleased]" stays, empty, above the new section with its entries.
    text = text[:body] + f"\n\n## [{version}] - {date}\n\n" + text[body:].lstrip("\n")
    # Compare links at the bottom.
    text = re.sub(r"^\[Unreleased\]: .*$", f"[Unreleased]: {REPO_URL}/compare/v{version}...HEAD", text, flags=re.M)
    link = f"{REPO_URL}/compare/v{previous}...v{version}" if previous else f"{REPO_URL}/releases/tag/v{version}"
    text = re.sub(r"^(\[Unreleased\]: .*)$", rf"\1\n[{version}]: {link}", text, count=1, flags=re.M)
    CHANGELOG.write_text(text)


def set_cargo(version):
    text, n = re.subn(r'^version = "[^"]+"', f'version = "{version}"', CARGO.read_text(), count=1, flags=re.M)
    if n != 1:
        fail("no version in client/linux/Cargo.toml")
    CARGO.write_text(text)
    text, n = re.subn(r'^(\[\[package\]\]\nname = "av-relay"\nversion = )"[^"]+"', rf'\1"{version}"', LOCK.read_text(), flags=re.M)
    if n != 1:
        fail("no av-relay package in client/linux/Cargo.lock")
    LOCK.write_text(text)


def set_metainfo(version, date):
    text = METAINFO.read_text()
    entry = f'<release version="{version}" date="{date}"/>'
    if re.search(rf'<release version="{re.escape(version)}"', text):
        # Already listed (the first release was written down ahead): date it.
        text = re.sub(rf'<release version="{re.escape(version)}"[^>]*/>', entry, text)
    else:
        text, n = re.subn(r"(<releases>\n)(\s*)", rf"\1\2{entry}\n\2", text, count=1)
        if n != 1:
            fail("no <releases> in the metainfo")
    METAINFO.write_text(text)


def git(*args, capture=False):
    result = subprocess.run(["git", "-C", str(ROOT), *args], check=True, text=True, capture_output=capture)
    return result.stdout.strip() if capture else None


def prepare(version, date, tag):
    if git("status", "--porcelain", capture=True):
        fail("the working tree has changes: commit or stash them first")
    branch = git("rev-parse", "--abbrev-ref", "HEAD", capture=True)
    if branch != "main":
        fail(f"on '{branch}': releases are made from main")
    if git("tag", "--list", f"v{version}", capture=True):
        fail(f"the tag v{version} exists already")
    set_changelog(version, date)
    set_cargo(version)
    set_metainfo(version, date)
    left = problems(version)
    if left:
        fail("after the update: " + "; ".join(left))
    git("add", str(CHANGELOG), str(CARGO), str(LOCK), str(METAINFO))
    git("commit", "-m", f"Release {version}")
    if tag:
        git("tag", "-a", f"v{version}", "-m", f"AV Relay {version}\n\n{notes(version)}")
        print(f"Tagged v{version}. Publish it with:\n\n    git push origin main v{version}\n")
    else:
        print(f"Committed. Tag it with: git tag -a v{version} -m 'AV Relay {version}'")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("version", help="X.Y.Z, or X.Y.Z-pre for a pre-release (a leading v is fine)")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="only check that the tree says this version everywhere")
    mode.add_argument("--notes", action="store_true", help="print this version's changelog entries")
    ap.add_argument("--date", default=datetime.date.today().isoformat(), help="release date (default: today)")
    ap.add_argument("--no-tag", action="store_true", help="commit, but don't tag")
    args = ap.parse_args()
    version = args.version[1:] if args.version.startswith("v") else args.version
    if not VERSION.match(version):
        fail(f"'{version}' isn't a version like 1.2.3 or 1.2.3-rc.1")
    if args.check:
        left = problems(version)
        if left:
            fail(f"the tree isn't ready for {version}: " + "; ".join(left) + ". Run scripts/release.py " + version + " on main.")
        print(f"{version}: Cargo.toml, Cargo.lock, metainfo and CHANGELOG.md agree")
    elif args.notes:
        entries = notes(version)
        if not entries:
            fail(f"CHANGELOG.md has no entries for {version}")
        print(entries)
        print(f"\nInstall: see the [README]({REPO_URL}#pick-your-installation). "
              f"All changes: [CHANGELOG.md]({REPO_URL}/blob/v{version}/CHANGELOG.md).")
    else:
        prepare(version, args.date, tag=not args.no_tag)


if __name__ == "__main__":
    main()
