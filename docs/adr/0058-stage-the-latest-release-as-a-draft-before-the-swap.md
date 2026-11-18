# 0058: Stage the latest release as a draft before the swap

- Status: Accepted
- Date: 2026-11-18

## Context

[ADR-0052](0052-publish-rolling-testing-builds-from-main.md) publishes each
tested merge to `main` as the rolling `latest` prerelease. Its publish job
deleted the previous release and tag first, then ran `gh release create latest`
with the archives. `gh` uploads to a draft and publishes it at the end. Nobody
saw a mix of old and new archives, but `releases/tag/latest` and every
`releases/download/latest/<asset>` link failed until all four uploads finished.
The first successful publish, run 44405313895 at commit `3208b03` on
2026-11-18, uploaded about 31 MB.

GitHub serves a download link through the one published release that owns the
tag. A release cannot change its assets atomically, and two published releases
cannot share a tag. A draft creates no tag, and only users with write access
can see it.

## Decision

The publish job in `.github/workflows/testing-build.yml` stages the new
release before it touches the published one. The
[repository-checks guide](../repository-checks.md#testing-build-workflow) owns
the details.

1. Run ADR-0052's ancestry check unchanged.
2. Delete any draft whose tag name starts with `testing-build-staging-`. These
   are left by a run that stopped before its cleanup ran. Main runs never
   overlap, so none belongs to a live run.
3. Create a draft prerelease under the run-specific tag name
   `testing-build-staging-<run id>-<run attempt>`, targeting the built
   commit, and upload the archives and `SHA256SUMS` to it.
4. Require exactly one such draft. Require its uploaded assets to match the
   local files by name and size.
5. Delete the previous `latest` release and tag. Then publish the draft
   through the release API with `tag_name: latest`, `draft: false`,
   `prerelease: true`, and `make_latest: "false"`. Publishing creates the tag
   at the target commit.
6. Confirm that `latest` resolves to the built commit.

An exit trap deletes the run's draft if the job fails before step 5 completes.
Until step 5 starts, the published release is never changed.

## Alternatives considered

- **Keep delete-then-upload.** Rejected. Downloads fail for the whole upload,
  which grows with the archives and the runner's upload speed.
- **Edit the published release in place with `--clobber` uploads.** Rejected
  again, as in ADR-0052. Readers could download a mix of old and new archives,
  and a failed upload would leave the release in that state.
- **Move the tag before deleting the old release.** Rejected. The swap has the
  same number of calls, and a failure part way would leave the old archives
  published under a tag that points at the new commit.
- **Retag the old release instead of deleting it.** Rejected. Publishing a
  release under a new tag name creates that tag, so each build would leave an
  extra tag. It also does not shorten the swap.
- **Stage under the tag name `latest`.** Rejected. GitHub does not document
  whether a draft may share a tag name with a published release, so the design
  does not rely on it. A run-specific name also identifies stale drafts.

## Consequences

- Downloads fail only during the swap: one release lookup, the delete, a tag
  lookup, and the publish call. The failure window is no longer tied to the
  upload. It is not zero, because GitHub has no atomic way to replace a
  release.
- If the publish call fails after the delete, no `latest` release exists and
  the trap deletes the draft. Re-running the failed publish job stages and
  publishes again from the same workflow artifacts, which are kept for seven
  days. ADR-0052's flow had the same failure mode.
- While a run uploads, users with write access see one extra draft release.
- The `testing-build-staging-` tag-name prefix is reserved for these drafts;
  the job deletes any draft that uses it.
- This change has run only against a stub `gh` in local tests. Check the first
  `main` run after it merges: the release, its assets, the tag target, and
  that no staging draft remains.

## References

- [ADR-0052: Publish rolling testing builds from main](0052-publish-rolling-testing-builds-from-main.md)
- [Repository checks](../repository-checks.md#testing-build-workflow)
- [REST API: Update a release](https://docs.github.com/en/rest/releases/releases#update-a-release)
- [GitHub CLI `release create`](https://cli.github.com/manual/gh_release_create)
