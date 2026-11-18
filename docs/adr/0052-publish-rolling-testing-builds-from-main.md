# 0052: Publish rolling testing builds from main

- Status: Accepted; amended by
  [ADR-0058](0058-stage-the-latest-release-as-a-draft-before-the-swap.md)
- Date: 2026-11-17

## Context

Every merge to `main` should produce downloadable builds of the prototype for
Windows, Ubuntu, and Fedora 44 without an installer. The builds must come only
from tested commits. The jobs that compile the workspace run third-party code:
Cargo build scripts, procedural macros, and the frontend's npm build. The
desktop shell has no packaging (`bundle.active` is `false`), and the
repository's [coding rules](../coding-rules.md) require pinned actions and no
elevated permissions for build jobs.

GitHub-hosted runners provide Windows and Ubuntu, but not Fedora. The Linux run
and tree-limit tests need a user service manager with delegated cgroups, and
the credential tests need an unlocked Secret Service. A job container provides
neither.

## Decision

`.github/workflows/testing-build.yml` runs on every push to `main` and on
manual dispatch. The [repository-checks guide](../repository-checks.md#testing-build-workflow)
owns the details.

- **Tests.** It calls `rust.yml` and `frontend.yml` as reusable workflows. The
  Rust leg runs on `ubuntu-latest` and Windows Server 2025, the same images
  that build those packages. Both workflows no longer trigger on a push to
  `main` directly, so each merge runs the suite once.
- **Packages.** Three jobs build `omgcode` and `omgcode-desktop` in release
  mode with the locked dependencies and the pinned toolchain: Windows Server
  2025, `ubuntu-latest`, and a Fedora 44 container pinned by image digest on an
  Ubuntu runner. Each job checks that both binaries start (`omgcode --version`;
  `omgcode-desktop` with an unknown option must exit with the usage code 2
  before a window opens) and archives them with the licence, the README, and
  a `BUILD-INFO.txt`. Windows produces a `.zip` and Linux a `.tar.gz`.
- **Publication.** A final job runs only from `main` after every test and
  package job succeeds. It is the only job with `contents: write`. It downloads
  the archives, writes `SHA256SUMS`, deletes the previous `latest` release and
  tag, and recreates them at the built commit as a prerelease that is never
  marked latest. It does not check out or run repository code.
- **Ordering.** Runs on a ref share one concurrency group and are never
  cancelled, so a publish cannot stop halfway. GitHub keeps only the newest
  waiting run, so the tag ends at the newest tested commit. The group includes
  the ref, so a manual run on another branch cannot displace a waiting main
  run. The publish job also refuses a commit that is not the published commit
  or ahead of it, so re-running an older run cannot roll the release back.

Fedora's archive is built and start-checked only. The workspace tests do not
run on Fedora, and the release notes and `BUILD-INFO.txt` say so.

## Alternatives considered

- **A third-party release action.** Rejected. The preinstalled `gh` CLI does
  the same with the job token, and avoids another pinned dependency with write
  access.
- **Editing the release in place with `--clobber` uploads.** Rejected. Readers
  could download a mix of old and new archives during the upload. Delete and
  recreate leaves a short window with no release instead, and `gh release
  create` uploads to a draft before publishing.
- **Running the Fedora tests in the container.** Rejected for now. The
  container has no user service manager or Secret Service, so the run,
  tree-limit, and credential tests would fail for environmental reasons.
  Excluding them would hide which checks ran. A Fedora virtual machine or a
  self-hosted runner can add them later.
- **Building packages after the tests finish.** Rejected. Building in parallel
  is faster, and publication still waits for both.
- **Tauri bundles (MSI, NSIS, deb, rpm, AppImage).** Out of scope. The request
  was for archives without an installer, and the shell has no signing or
  packaging design.

## Consequences

- Stable download URLs:
  `releases/download/latest/omgcode-<platform>.<zip|tar.gz>`. The tag is named
  `latest`, but it stays a prerelease that is not marked latest, because the
  prototype is not release-ready. GitHub's `/releases/latest` route therefore
  does not resolve to it.
- The archives are unsigned prototype builds. They carry no claim of release
  qualification, sandbox containment, or platform support beyond the
  [platform support guide](../platform-support.md).
- Main's check names change from `Rust (...)` to the nested
  `Testing build / Rust tests / Rust (...)` form.
- `ubuntu-latest` moves when GitHub changes its default image. The Ubuntu
  archive's glibc and WebKitGTK requirements move with it.
- The Fedora image digest freezes the base packages until someone updates it.
- If the repository enables immutable releases, deleting and recreating the
  `latest` tag will fail. Revisit this decision before enabling that setting.
- None of this has run on GitHub. Check the first run on `main`, the release
  assets, and the tag target before relying on it.

## References

- [Repository checks](../repository-checks.md#testing-build-workflow)
- [Reusable workflows](https://docs.github.com/en/actions/sharing-automations/reusing-workflows)
- [GitHub CLI `release create`](https://cli.github.com/manual/gh_release_create)
- [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/)
- [ADR-0004: Require pull requests and repository analysis](0004-require-pull-requests-and-repository-analysis.md)
- [ADR-0045: Build the desktop shell on Tauri](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
