# Releasing

Releases are made by hand from GitHub: **Actions → Release → Run workflow**,
on the default branch. Nothing else is needed — no tag to create, no version
to edit.

| Input | Meaning |
|---|---|
| `bump` | `patch` (0.1.0 → 0.1.1), `minor` (→ 0.2.0) or `major` (→ 1.0.0), counted from the newest `vX.Y.Z` tag |
| `version` | Optional exact version (`1.2.0`, `1.2.0-beta.1`); wins over `bump`. Must be new and not older than the latest release |
| `prerelease` | Mark the GitHub release as a pre-release (a `-suffix` version is one automatically) |
| `draft` | Create a draft to look over and publish yourself |
| `dry_run` | Build and package only; the files are kept as a workflow artifact for 7 days. No commit, tag or release |

The very first release (no `v*` tag yet) uses the version already in
`Cargo.toml` as it is.

## What it does

The workflow is two jobs: a build matrix (Windows / Linux / macOS runners
each working out the same version and running the gates), and a `publish`
job that collects the packages.

1. Each build runner works out the version (`scripts/release-version.sh
   next …`, fetches tags first) and writes it into `Cargo.toml` /
   `Cargo.lock` (`… apply …`).
2. Builds the UI, then runs the same checks as CI: `cargo fmt --check`,
   `cargo clippy -D warnings`, `cargo test`.
3. `cargo build --release --locked`, then packs, per runner:
   - `windows-latest`: `push-to-talk-for-anywhere-vX.Y.Z-windows-x64.zip`
     (`push-to-talk-for-anywhere.exe`, `ptt.exe`, README) plus the bare
     `push-to-talk-for-anywhere-vX.Y.Z-windows-x64.exe`,
   - `ubuntu-22.04`: `push-to-talk-for-anywhere-vX.Y.Z-linux-x64.tar.gz`
     (`push-to-talk-for-anywhere`, `ptt`, README),
   - `macos-latest`: `push-to-talk-for-anywhere-vX.Y.Z-macos-arm64.zip`
     (`push-to-talk-for-anywhere`, `ptt`, README).
   The tar.gz/zip are plain, runnable archives.
4. Only if every runner passed: the `publish` job merges the packages into
   one `SHA256SUMS.txt` (also covering the Windows-only files), and commits
   `Release vX.Y.Z` (the version bump) to the branch, then creates the
   `vX.Y.Z` tag on that commit and the GitHub release with generated notes
   and the files attached.

If a step fails nothing is released. If it fails after the commit but before
the release exists, run the workflow again: it arrives at the same version,
finds `Cargo.toml` already updated and finishes the job.

## Requirements

- The workflow needs `contents: write` (it declares it). If the default
  branch is protected so that the Actions bot cannot push to it, either allow
  it to bypass the rule or push the version bump with a personal access token
  stored as a secret.
- The release job runs as a matrix on `windows-latest`, `ubuntu-22.04` and
  `macos-latest`; all three gates must pass on all platforms. The Linux
  runner installs the standard Tauri system packages before it builds;
  the Windows and macOS runners need nothing extra.

Each platform builds and gates independently. If one runner fails, nothing is
released — re-run and the surviving work is reused from cache. There is no
`.deb`/`.app` bundling yet (`cargo tauri bundle`): a Linux `.deb` and a macOS
zip-with-`.app` are follow-ups; the tar.gz/zip archives are plain and runnable
as they are.
