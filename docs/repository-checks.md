# Repository checks and branch protection

## Main branch

The active repository ruleset requires every update to `main` to arrive through
a pull request. It also blocks force-pushes and branch deletion.

- Ruleset: `main: pull requests only` (ID `26780301`).
- Target: `refs/heads/main`.
- Bypass actors: none, including no administrator bypass entry.
- Required approving reviews: zero.
- Required named status checks: none in this initial PR-only rule.

A pull request is required even for maintainers and administrators while the
rule is active. Repository administrators can still edit repository rules;
this setting does not remove their administrative authority.

Reviews and analysis results are visible on pull requests. Adding mandatory
approvals or named checks is a separate policy change. Do not add an unverified
or unavailable external check that would block every merge.

Use feature branches and merge through GitHub. Do not temporarily disable this
rule to publish workflow or documentation changes.

## CodeRabbit

The organization installation already covers this repository. The root
`.coderabbit.yaml` enables automatic and incremental reviews without the
default pause after a fixed number of reviewed commits. Its guideline list
includes contributor, coding, security, and testing rules.
The guides stored under docs are explicitly mapped to all repository paths;
their directory location must not limit application-code reviews.

A valid file or installed app is not proof of a review. Verify a real review on
the current pull-request head. CodeRabbit feedback needs human or coordinator
judgment; its presence does not establish that a change is correct.

## CodeQL

`.github/workflows/codeql.yml` runs for pull requests to main, pushes to main,
manual dispatch, and a weekly schedule.

The workflow always analyzes GitHub Actions. It adds Rust when tracked Rust
source and a Cargo manifest or rust-project.json exist, and JavaScript/TypeScript
when matching tracked source files exist. Rust source without an analysis
manifest fails language selection rather than being silently omitted.

Manifest discovery is repository-wide, not proof that every Rust file belongs
to a Cargo target. Inspect extraction diagnostics when source is introduced.
A successful selection or scan does not prove a full Cargo build or complete
semantic coverage of snippets outside the application project.

JavaScript detection excludes the extractor's default node_modules and
bower_components directories. The supported standalone extensions are .js,
.jsx, .ts, .tsx, .mjs, .cjs, .mts, and .cts. HTML-only inline scripts do not
activate this language in the current detector. Revisit that scope if the
frontend uses inline JavaScript; do not infer coverage from a green Actions scan.

Each selected language uses the no-build mode and security-extended queries.
No application build is claimed. CodeQL's Rust processing can still execute
build scripts or procedural macros; the analysis job has no Sonar or provider
secrets. Keep its permissions restricted.

The repository now contains Rust source in the core, CLI, platform, application,
and desktop crates.
CodeQL selects Rust when tracked Rust files and a Cargo manifest are present.
Its no-build scan is separate from the locked Cargo checks in
`.github/workflows/rust.yml`; it is not a full application build or proof of
application security. The desktop frontend adds tracked `.ts` files under
`crates/omgcode-desktop/ui`, so the selection rule above now adds
JavaScript/TypeScript. That is expected from the rule and is unverified until a
CodeQL run completes on a pull request. `node_modules` and the generated `dist`
directory are ignored by Git and the extractor. Add other source languages
deliberately when the implementation needs them.

## Rust and Frontend workflows

`.github/workflows/rust.yml` runs on Ubuntu 24.04 and Windows Server 2025 with a
40 minute timeout (it was 20). Before Clippy it installs the Tauri build
packages on Ubuntu (`libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`,
`file`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev`, and
`librsvg2-dev`), sets up Node 24 with `actions/setup-node` pinned to a full
commit (v7.1.0), and runs `npm ci` and `npm run build` in
`crates/omgcode-desktop/ui`. The desktop crate embeds `ui/dist` at compile time,
so every workspace-wide Cargo step needs that build first.

Both workflows run on pull requests to main and manual dispatch. They no longer
trigger on a push to main; the [Testing build workflow](#testing-build-workflow)
calls them as reusable workflows instead, so each merge runs them once. The Rust
workflow takes a `linux-runner` input (default `ubuntu-24.04`) for its Linux
leg. Their concurrency groups start with `github.workflow`, which names the
caller in a called workflow, so a direct run cannot cancel a Testing build run.

`.github/workflows/frontend.yml` runs on Ubuntu 24.04 and Windows Server 2025. It
runs `npm ci`, `npm run check`, `npm run lint`, `npm test`, and
`npm run test:dist`. `.npmrc` sets `ignore-scripts=true`, so no dependency
install script runs.

`.gitattributes` sets `crates/omgcode-desktop/ui/** text=auto eol=lf` so a CRLF
checkout on Windows does not fail Biome's format check.

Both workflows have run on GitHub. In Testing build run 44405313895 (commit
`3208b03`, 2026-11-18), the Rust and Frontend legs passed on Ubuntu and
Windows Server 2025. A passing run shows that the exercised checks passed on
those images, not that the workflows cover every platform behaviour.

On Ubuntu, the Rust workflow enables lingering for the runner user and waits
for its user service manager before the tests. The run and tree-limit tests
start delegated `systemd-run --user` scopes and fail without one.

Every apt step in `rust.yml` and `testing-build.yml` runs
`.github/scripts/apt-install-ubuntu.sh`, which points apt at the image's
`/etc/apt/sources.list.d/ubuntu.sources` and no other source file. A
third-party repository that the runner image adds, such as
`packages.microsoft.com`, cannot fail the step; it returned `403` and failed
a zfs job before its tests on 2026-11-27 (run 46051772270). An error from the
Ubuntu archive still fails the step, and so does a missing source file.

The Linux credential tests need an unlocked Secret Service.
`.github/scripts/start-secret-service.sh` installs gnome-keyring and runs
`gnome-keyring-daemon --foreground --unlock --components=secrets` under a
background subshell that writes the daemon's output and exit status to
`$RUNNER_TEMP/secret-service/daemon.log`. It waits through the D-Bus driver,
which never starts a service, until that daemon owns
`org.freedesktop.secrets`, and fails if another process owns the name. When a
later step fails, `.github/scripts/secret-service-diagnostics.sh` prints that
log, whether the daemon still runs, the current name owner, journal entries
for gnome-keyring, the session bus, and systemd-oomd, the daemon's core dumps
through `coredumpctl` (the runner's `core_pattern` is systemd-coredump), and
kernel OOM and crash lines. In the second attempt of that run the daemon left the session bus
partway through the core tests, D-Bus activated the packaged
`gnome-keyring-daemon.service` with a locked login collection, and 28
credential tests failed with `Locked`. Its cause is unknown. The scripts only
record evidence; no test is retried or serialised.

## Testing build workflow

`.github/workflows/testing-build.yml` runs on every push to main and on manual
dispatch. It replaces the rolling `latest` prerelease with archives of the
newest tested commit ([ADR-0052](adr/0052-publish-rolling-testing-builds-from-main.md)).

| Job | Runner | Work |
| --- | --- | --- |
| Rust tests | `ubuntu-latest`, Windows Server 2025 | `rust.yml`, called with `linux-runner: ubuntu-latest` |
| Frontend tests | Ubuntu 24.04, Windows Server 2025 | `frontend.yml` |
| Package (windows-x86_64) | Windows Server 2025 | Release build, start-up check, `.zip` |
| Package (ubuntu-x86_64) | `ubuntu-latest` | Release build, start-up check, `.tar.gz` |
| Package (fedora44-x86_64) | Fedora 44 container on Ubuntu 24.04 | Release build, start-up check, `.tar.gz` |
| Publish the latest release | Ubuntu 24.04 | Checksums, staged draft, and release swap |

Each package job builds the frontend, then runs
`cargo build --release --locked -p omgcode-cli -p omgcode-desktop` with the
pinned toolchain. The start-up check runs `omgcode --version` and requires
`omgcode-desktop --unknown-option` to exit with the usage code 2, which happens
before any window opens. On Linux this also shows that the dynamic loader found
WebKitGTK. Each archive holds both binaries, `LICENSE`, `README.md`, and a
`BUILD-INFO.txt` with the version, commit, run URL, build OS, and toolchain.
There is no installer and nothing is signed. The workflow artifacts are kept
for seven days; the release holds the lasting copies.

The Fedora job uses `registry.fedoraproject.org/fedora:44` pinned by its image
index digest, recorded on 2026-11-17. It installs Git, the archive tools, GCC,
Fedora's `rustup` package, and the Tauri prerequisites for Fedora with `dnf`,
then installs the pinned toolchain through `rustup-init`. Update the digest
deliberately to take newer Fedora packages. The workspace tests do not run on
Fedora: the container has no user service manager for delegated cgroups and no
Secret Service. The release notes and `BUILD-INFO.txt` state this.

The publish job needs every test and package job and runs only for
`refs/heads/main`, so a manual run on another branch builds but publishes
nothing. It is the only job with `contents: write`. It never checks out or runs
repository or dependency code. It downloads the archives and writes
`SHA256SUMS`. It then stages and swaps the release
([ADR-0058](adr/0058-stage-the-latest-release-as-a-draft-before-the-swap.md)):

1. It deletes any draft whose tag name starts with `testing-build-staging-`.
   Such a draft is left by a run that stopped before its cleanup ran. The
   prefix is reserved for these drafts.
2. It runs `gh release create testing-build-staging-<run id>-<attempt> --draft
   --target <commit> --prerelease --latest=false` with the assets. A draft
   creates no tag, and only users with write access see it. The published
   `latest` release keeps serving its archives during the upload.
3. It requires exactly one such draft whose uploaded assets match the local
   files by name and size.
4. It deletes the previous `latest` release, then deletes the `latest` tag
   until an exact lookup no longer finds it. A lookup just after the delete
   can still list the tag, and a second delete then fails with HTTP 422
   "Reference does not exist", so a failed delete is followed by another
   lookup. After five delete attempts, about 30 seconds of waits, the job
   fails. A failed lookup also fails it. The job then publishes the draft
   through the release API with `tag_name: latest`, `draft: false`,
   `prerelease: true`, and `make_latest: "false"`. Publishing creates the tag
   at the built commit.
5. It checks that `latest` resolves to the built commit and prints the
   published assets.

Nobody sees a mix of old and new archives. Downloads fail only between the
delete and the publish call in step 4, not for the whole upload. An exit trap
deletes the run's draft if the job fails before the publish call succeeds. If
the publish call itself fails, no `latest` release exists until a re-run of the
publish job, which stages again from the same artifacts.

Runs use the concurrency group `testing-build-<ref>` with `cancel-in-progress:
false`. A publish is never cancelled part way. GitHub keeps only the newest
waiting run in a group, so the tag ends at the newest main commit whose tests
passed. Because the group includes the ref, a manual run on another branch
cannot replace a waiting main run.

Before deleting anything, the publish job resolves the commit that `latest`
points to and compares it with the run's commit through the compare API. It
continues only when the two are the same commit or the run's commit is ahead.
A re-run of an older main run keeps its original commit, so without this check
it could roll the release back; with it, the job fails and leaves the newer
release in place. The check fails closed: it looks the tag up with
`git/matching-refs`, which returns an empty list when the tag is absent, so only
a confirmed missing tag skips the comparison. Any API error stops the job
before anything is deleted.

The assets have stable names, so links do not change between builds:
`https://github.com/OMG-Software/OMGCode/releases/download/latest/omgcode-windows-x86_64.zip`,
`.../omgcode-ubuntu-x86_64.tar.gz`,
`.../omgcode-fedora44-x86_64.tar.gz`, and `.../SHA256SUMS`.

The tag is named `latest`, but GitHub's own `/releases/latest` route and the
release list's "Latest" badge select the newest release that is not a
prerelease. This release is a prerelease created with `--latest=false`, so
those do not point to it. Link to `releases/tag/latest` or
`releases/download/latest/<asset>` instead.

Limits:

- The delete-then-create publish first succeeded in run 44405313895 at
  `3208b03` on 2026-11-18; earlier main runs failed in the Ubuntu tests. The
  staged-draft swap has run only against a stub `gh` in local tests. Inspect
  the first main run after it merges: the release, its assets, the tag target,
  and that no `testing-build-staging-` draft remains.
- The swap is not atomic. GitHub cannot replace a published release in one
  call, so downloads fail briefly between the delete and the publish.
- `ubuntu-latest` moves when GitHub changes its default image, and the Ubuntu
  archive's glibc and WebKitGTK requirements move with it.
- Enabling immutable releases would block reusing the `latest` tag. Revisit
  ADR-0052 first.
- A start-up check is not a platform qualification. The
  [platform support guide](platform-support.md) remains the support statement.

## SonarQube Cloud

Project: [OMGCode](https://sonarcloud.io/project/overview?id=OMG-Software_OMGCode)
in the `omg-software` organization.

The analysis project key is `OMG-Software_OMGCode`. A Sonar administrator
changed it from `OMG-Software_OrangeCode` on 2026-11-11. The key identifies the
analysis history; it is not an application namespace. A GitHub repository
rename does not migrate a Sonar project key, so change the key on the server
before changing `sonar-project.properties`. The workflow requires a completed
analysis on the current pull-request head.

`.github/workflows/sonar.yml` uses CI-based analysis and
`sonar-project.properties`. Keep Sonar Automatic Analysis disabled to avoid
conflicting analysis methods.

Configure the `SONAR_TOKEN` repository secret with the intended project's
analysis credential. Never commit or print the token. The job fails explicitly
if the credential is missing. It waits for the server's quality-gate result.

To set up a personal analysis token:

1. Sign in to SonarQube Cloud using the GitHub account with analysis permission.
2. Open My account > Access Tokens > Personal Tokens.
3. Generate a token named omgcode-github-actions with an explicit expiry.
4. Copy it directly to GitHub Settings > Secrets and variables > Actions >
   New repository secret, with the name SONAR_TOKEN.

The token is created in Sonar, not GitHub. Do not paste it into a chat or commit
it. If the organization plan provides scoped organization tokens, prefer a
token restricted to this project's analysis and required read access.

Dependabot-triggered workflows use Dependabot secrets, not Actions secrets.
If Dependabot is enabled, configure SONAR_TOKEN in GitHub's Dependabot secret
store too. Without it, the credential check intentionally fails for those runs.

Fork pull requests do not receive the configured Sonar secret and skip this job.
Their other actions can still access a limited GITHUB_TOKEN. Do not use
`pull_request_target` to run fork code with the Sonar credential.
Use a trusted, separately reviewed path when a fork requires a Sonar analysis.

The source root is the repository, including Rust crates, workflow, and tooling
files. The explicit exclusions cover .git, target, node_modules, dist, and
coverage directories at any depth. They do not cover every generated directory;
review exclusions as the repository grows. Use the Rust workflow for compilation
and Clippy checks. The Rust workflow reports no coverage.

Test code is declared to Sonar as test code: every `tests` directory, every
`tests.rs` module, and every `*.test.ts` file. The same patterns are excluded
from the sources, because a file cannot be both. Sonar then applies its test
rules to those files and does not count them as code to cover or measure them
for duplication. Rust unit tests written inside a source file in a
`#[cfg(test)]` module cannot be separated this way and are still analysed as
source.

Sonar counts the frontend's TypeScript sources as code to cover, so the Sonar
workflow gives it a coverage report. A first job, `Frontend coverage`, installs
the locked dependencies, runs `npm run coverage`, and uploads `lcov.info` as an
artifact kept for one day. The analysis job waits for it and downloads the
report to `crates/omgcode-desktop/ui/coverage`, where
`sonar.javascript.lcov.reportPaths` points. The report's paths are relative to
the repository root. The tests run dependency code, so they run in a job that
never references the Sonar credential; the analysis job receives only the
report. Keep it that way.

The coverage provider is `@vitest/coverage-v8`, pinned to the Vitest version.
The report covers every file under `ui/src`, including the development mock,
with no exclusion. On 2026-11-17 it measured 85.7% of lines and 76.0% of
branches, which is about 82% by Sonar's combined measure against a required
80%. That margin is thin: new frontend code needs tests, or the gate fails.
Rust is not measured for coverage at all. Do not read a passing gate as
evidence of Rust test coverage.

Both workflows install the frontend's dependencies with
`npm ci --ignore-scripts`. The flag repeats what `ui/.npmrc` sets, so no
package install script runs even if that file changes. Source and tests name
bidirectional control characters with `\u` escapes, never as raw characters; a
frontend test scans for raw ones.

Check the current Sonar result and quality gate for each pull request. Do not
present a missing, empty, or failed analysis as proof that application code
passed.

## Maintenance and verification

- Third-party actions use verified full commit pins with release comments.
  Review vendor releases before updating them.
- Check YAML, the CodeRabbit schema, workflow syntax, and changed documentation.
  Run the frontend gates locally when `crates/omgcode-desktop/ui` changes.
  Verify CodeQL language selection for absent and newly introduced source.
- After publishing through a pull request, inspect the actual review and
  completed workflow results. Check the head commit and final exit statuses.
- Confirm the effective main rules through the GitHub API. Do not test direct
  push rejection by attempting to land an unreviewed commit on main.
- Keep the [testing standards](testing-standards.md) and this guide accurate as
  application checks change. These integrations do not replace runtime,
  sandbox, credential, or platform verification.

## References

- [CodeRabbit configuration](https://docs.coderabbit.ai/reference/configuration)
- [CodeQL workflow options](https://docs.github.com/en/code-security/reference/code-scanning/workflow-configuration-options)
- [Sonar GitHub setup](https://docs.sonarsource.com/sonarqube-cloud/getting-started/github)
- [Sonar analysis methods](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/automatic-analysis)
- [Sonar personal tokens](https://docs.sonarsource.com/sonarqube-cloud/managing-your-account/managing-tokens)
- [Dependabot workflow credentials](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-on-actions)
- [Branch rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
