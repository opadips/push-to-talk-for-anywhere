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

1. Works out the version (`scripts/release-version.sh next …`) and writes it
   into `Cargo.toml` / `Cargo.lock` (`… apply …`).
2. Builds the UI, then runs the same checks as CI: `cargo fmt --check`,
   `cargo clippy -D warnings`, `cargo test`.
3. `cargo build --release --locked`, then packs
   `ptt-tool-vX.Y.Z-windows-x64.zip` (`ptt-tool.exe`, `ptt.exe`, README),
   the bare `ptt-tool-vX.Y.Z-windows-x64.exe` and `SHA256SUMS.txt`.
4. Only if all of that passed: commits `Release vX.Y.Z` (the version bump) to
   the branch, then creates the `vX.Y.Z` tag on that commit and the GitHub
   release with generated notes and the files attached.

If a step fails nothing is released. If it fails after the commit but before
the release exists, run the workflow again: it arrives at the same version,
finds `Cargo.toml` already updated and finishes the job.

## Requirements

- The workflow needs `contents: write` (it declares it). If the default
  branch is protected so that the Actions bot cannot push to it, either allow
  it to bypass the rule or push the version bump with a personal access token
  stored as a secret.
- Windows only: the app, the hooks and the audio code are Windows-only, so the
  release job runs on `windows-latest`.
