# Releasing

[CHANGELOG.md](../CHANGELOG.md) is the source of release notes. The repository uses
[cargo-release](https://github.com/crate-ci/cargo-release) for version updates,
changelog rollover, signed commits and tags, and pushing to `origin`.
[release.toml](../release.toml) restricts releases to `master` and disables Cargo
registry publishing. GitHub release publication is a separate final step.

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
- GitHub CLI authenticated with permission to publish this repository's releases.
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
commit, tag, and push. The pre-release hook runs Rust tests, analytics type
checking and tests, a release build, and the binary's `--version` command. Any
failed check stops the release before the release commit is created.

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

It does not publish to crates.io, create a GitHub release, or upload binaries.

## Publish the GitHub release

Copy only the released version's changelog body into `/tmp/release-notes.md`,
excluding the version heading, other releases, and the reference links at the
bottom of the changelog. Append a full changelog link using the previous and new
tags. Review the file, then publish it:

```bash
gh release create v0.4.1 \
  --repo InvictusNavarchus/keyboard-debouncer \
  --verify-tag \
  --title v0.4.1 \
  --notes-file /tmp/release-notes.md

gh release view v0.4.1 \
  --repo InvictusNavarchus/keyboard-debouncer
```

For a prerelease, add `--prerelease` to `gh release create`.

If GitHub publication fails after the tag was pushed, retry only the publication
step using the existing tag. If an earlier step fails, inspect `git status`, the
current version, local tags, and remote refs before retrying: `cargo-release` may
already have applied some changes. Preserve reviewed notes and do not recreate
or move a published version tag.

## Automatic binary builds

Publishing a release triggers [Release binaries](../.github/workflows/release.yml).
The workflow checks out the release tag, checks its Cargo version, installs the
latest Bun 1.4.x patch, and runs the analytics type check and tests with Bun.
It tests and builds the daemon for `x86_64-unknown-linux-musl` with stable Rust,
then verifies the executable's version, help output, and static linkage.

The workflow attaches a Linux x86_64 installation archive and its `.sha256` file
to the existing release. The archive contains the binary, installer, uninstaller,
systemd unit, example configuration, documentation, license, and Bun reporting
helpers, plus the source from that exact tag. GitHub also supplies the automatic
source archives. Binary assets appear after the workflow finishes; publication
initially shows only the source archives.

The job uses GitHub's built-in token with `contents: write`; no additional secret
is required. Project JavaScript/TypeScript commands use Bun, including forcing the
type checker to run with Bun. Standard checkout/setup actions use GitHub's own
Node action runtime internally.

The workflow must be on the default branch before a manual run. To build assets
for an existing published release, such as v0.4.0, without moving its tag:

```bash
gh workflow run release.yml \
  --repo InvictusNavarchus/keyboard-debouncer \
  --ref master \
  -f tag=v0.4.0

gh run list \
  --repo InvictusNavarchus/keyboard-debouncer \
  --workflow release.yml
```

Manual runs use the workflow from `master` and build the source at the requested
tag. A draft release must be published first. Immutable releases cannot accept
assets after publication and are rejected by this workflow.

Existing assets are never overwritten. After a failed build, rerun its workflow.
If a partial upload left assets behind, inspect them before deliberately removing
the affected archive/checksum and retrying. CI checks do not exercise a physical
keyboard; verify device interception on hardware before shipping behavior changes.
