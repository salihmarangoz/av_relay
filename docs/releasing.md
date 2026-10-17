# Releasing

The sender and the receiver are released together, with one version number,
from `main`. Pushing a version tag makes the release: GitHub Actions builds
the sender and both receivers and publishes them with the changelog's notes.

## Versions

[Semantic Versioning](https://semver.org/spec/v2.0.0.html), `MAJOR.MINOR.PATCH`:

- **PATCH** (0.1.1): fixes only.
- **MINOR** (0.2.0): new features; a sender and a receiver of different
  minor versions still work together.
- **MAJOR** (1.0.0): a sender and a receiver of the new version no longer work
  with older ones, e.g. a new protocol version (`av-relay/2`). Before 1.0.0,
  a MINOR step may do that too; the changelog then says so.
- **Pre-release**: `0.2.0-rc.1`, published as a pre-release on GitHub.

The version lives in both receivers' `Cargo.toml` and `Cargo.lock` files and
the Linux AppStream metainfo; the Mac app gets it from the tag when
it's built. `scripts/release.py` sets all of them, so don't edit them by hand.

## While working

Every change that a user would notice gets a line in `CHANGELOG.md` under
`## [Unreleased]`, in the same commit or branch, under **Added**, **Changed**,
**Fixed** or **Removed** ([Keep a Changelog](https://keepachangelog.com/en/1.1.0/)).
Write for users: what they see or can do now, not how the code changed.

## Making a release

1. On `main`, up to date, with CI green, and the changelog's
   `## [Unreleased]` saying what's new:

   ```
   scripts/release.py 0.2.0
   ```

   It moves the Unreleased entries under `## [0.2.0] - <today>`, sets the
   version in both receiver packages, commits "Release 0.2.0" and makes the
   annotated tag `v0.2.0`. Nothing leaves your computer yet.

2. Look at the commit (`git show`), then publish:

   ```
   git push origin main v0.2.0
   ```

3. The **Release** workflow (`.github/workflows/release.yml`) checks that the
   tag, the package version and the changelog agree, runs the tests, builds
   the Mac `.zip` and `.dmg`, Linux `.deb`, Windows x64 installer and checksum,
   and creates the GitHub
   Release with the changelog's section as its notes. Follow it under
   **Actions**; it takes about 15 minutes.

## If something goes wrong

- **The check fails** ("the tree isn't ready"): the tag was made by hand, or
  something changed the version files. Delete the tag
  (`git push --delete origin v0.2.0 && git tag -d v0.2.0`), fix `main`, and
  run `scripts/release.py` again.
- **A build fails after the check**: nothing has been published. Fix it on
  `main`, delete the tag as above, and release the fix as the next patch
  version (0.2.1) or move the tag to the fixed commit if the release never
  went out.
- **A published release is broken**: don't replace its files. Release a fix
  as the next patch version.

## Signing

Releases are signed ad hoc (no Apple Developer ID): no keys or secrets are
needed in CI. Users confirm the first launch once in System Settings, and
macOS asks for camera and microphone permission again after each update; see
[docs/macos.md](macos.md).

The Windows installer is unsigned. Windows CI bundles the required GStreamer
runtime DLLs and plugins, the MSVC runtime, and license notices. OBS and VB-CABLE
drivers are installed separately. CI tests installation and uninstallation
before uploading the installer; a failed platform build prevents publication.
