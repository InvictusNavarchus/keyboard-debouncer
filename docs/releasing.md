# Releasing

[CHANGELOG.md](../CHANGELOG.md) is the source of release notes. The repository uses
[cargo-release](https://github.com/crate-ci/cargo-release) for version updates,
changelog rollover, signed commits and tags, and pushing to `origin`.
[release.toml](../release.toml) restricts releases to `master` and disables Cargo
registry publishing. Pushing a version tag starts GitHub Actions, which builds
and validates the installation archive before publishing the GitHub release.

## Record changes during development

Add user-visible changes under `## [Unreleased]`, alongside the feature or fix.
Use `### Added`, `### Changed`, `### Fixed`, or `### Removed` as needed. Include
`### Breaking Changes` and `### Upgrading` when users must take action.

Explain the resulting behavior and why it matters to users. Routine internal
refactoring does not need an entry. Review the accumulated notes before choosing
the next version; do not derive the release notes solely from commit subjects.

The v0.3.0 and v0.4.0 entries were copied from their published GitHub releases.
Older Git tags have no corresponding published release notes in this changelog.

## Prerequisites

- Linux with the Rust toolchain and C build tools described in the
  [README](../README.md#prerequisites).
- Bun and the analytics development dependencies:
  ```bash
  cd scripts
  bun install --frozen-lockfile
  ```
- `cargo-release` (this configuration was validated with 1.1.7).
- Git commit/tag signing configured with an available signing key.
- Permission to push `master` and version tags to `origin`.

Run the following release commands from the repository root.

## Prepare and preview

Merge the intended changes into `master`, finalize and commit the `Unreleased`
notes, then update the branch and confirm the working tree is clean:

```bash
git switch master
git pull --ff-only
git status --short --branch
```

Choose an explicit version. The commands below use `0.4.1` as an example;
substitute the intended version throughout. Do not edit the Cargo version or
rename the `Unreleased` heading manually.

```bash
cargo release 0.4.1
```

This defaults to a dry run. Review the planned version, changelog replacements,
commit, tag, and push. The pre-release hook refuses missing or empty release
notes, then runs Rust tests, analytics type checking and tests, a release build,
and the binary's `--version` command. Any failed check stops the release before
the release commit is created.

Dry runs leave the version and changelog unchanged, so their checks validate the
current checkout. Execution repeats the checks after applying the version and
changelog updates. The hook never starts the daemon or installs a service.

## Create the release commit and tag

```bash
cargo release 0.4.1 --execute
```

Review the confirmation prompt. The configured process:

1. Updates `Cargo.toml` and `Cargo.lock`.
2. Moves the accumulated notes under a dated version heading, inserts a fresh
   `Unreleased` heading, and updates the changelog comparison links.
3. Runs the pre-release checks.
4. Creates a signed `chore(release): <version>` commit and signed `v<version>` tag.
5. Pushes the commit and tag to `origin`.

The pushed tag starts the release workflow. Cargo registry publishing is disabled.

## Publish the GitHub release

Wait for [Release](../.github/workflows/release.yml) to finish. It builds and
validates the tagged source, extracts that version's changelog body with
[`scripts/release-notes.sh`](../scripts/release-notes.sh), and assembles the
archive and checksum on a draft release. It publishes only after both assets
have uploaded successfully. No manual note copying or publication is needed.

Do not create or publish the release in the GitHub web UI. The workflow owns
publication and refuses to modify an already published release. Tags with a
prerelease version, such as `v0.5.0-rc.1`, produce GitHub prereleases.

You can inspect the workflow and resulting release using an authenticated GitHub
CLI:

```bash
gh run list \
  --repo InvictusNavarchus/keyboard-debouncer \
  --workflow release.yml

gh release view v0.4.1 \
  --repo InvictusNavarchus/keyboard-debouncer
```

If an upload or publication step fails, rerun the failed workflow jobs. The
publisher resumes an existing draft and replaces its incomplete assets before
publishing. A failed build leaves nothing public. If a fix requires a new code
commit, include the unshipped notes in the next version and release that version;
do not move a published tag. If a local release step fails, inspect `git status`,
the current version, local tags, and remote refs before retrying: `cargo-release`
may already have applied some changes.

## Automatic binary builds

Pushing a `v*` tag triggers [Release](../.github/workflows/release.yml).
The workflow checks out that tag, checks its Cargo version, installs the
latest Bun 1.4.x patch, and runs the analytics type check and tests with Bun.
It tests and builds the daemon for `x86_64-unknown-linux-musl` with stable Rust,
then verifies the executable's version, help output, and static linkage.

The workflow attaches a Linux x86_64 installation archive and its `.sha256` file
to the draft release. The archive contains the binary, installer, uninstaller,
systemd unit, example configuration, documentation, license, and Bun reporting
helpers, plus the source from that exact tag. GitHub also supplies the automatic
source archives. All assets are attached before publication, including when
GitHub immutable releases are enabled.

Builds use read access; only the publication job receives `contents: write` on
GitHub's built-in token. No additional secret is required. Project
JavaScript/TypeScript commands use Bun, including forcing the
type checker to run with Bun. Standard checkout/setup actions use GitHub's own
Node action runtime internally.

The workflow must be on the default branch before a manual run. Rehearse the
build from `master` without creating a tag or publishing a release:

```bash
gh workflow run release.yml \
  --repo InvictusNavarchus/keyboard-debouncer \
  --ref master

gh run list \
  --repo InvictusNavarchus/keyboard-debouncer \
  --workflow release.yml
```

Manual runs on a branch upload the archive and checksum as Actions artifacts and
skip publication. They validate the checked-out Cargo version without requiring a
version tag. CI checks do not exercise a physical keyboard; verify device
interception on hardware before shipping behavior changes.
