# Releasing Norn (alpha)

`.github/workflows/release.yml` builds every release. It is started by hand and does nothing public unless the owner approves it.

## What the workflow does

1. **Build job.** Builds from the dispatched commit:
   - the checker WebAssembly module;
   - the Linux dev hosts (x64 and arm64);
   - the third-party licence file;
   - the GitHub pre-release files and the npm packages.

   `scripts/release/build.sh` drives this with the tools pinned in `scripts/release/tools.sh`. The job then runs `assemble.py verify`, installs the packaged CLI, checks that `norn --version` names this commit, and uploads everything as the workflow artifact `release`, kept 14 days.

   A dry run (`publish: none`) ends here. The artifact can be downloaded by anyone who can read the repository. Nothing else is created.
2. **Publish job.** Runs only when all of these hold:
   - the dispatch asked to publish (`github` or `github-and-npm`);
   - it runs from `main`;
   - `confirm-version` equals the version in `tools/norn-ts/package.json`;
   - that version is not tagged yet;
   - the `release` environment has required reviewers and a branch rule. The build job checks this before anything else.

   Then it waits until a reviewer approves the deployment.

   After approval it:
   - re-verifies the files against the commit;
   - attests them;
   - creates the pre-release;
   - with `github-and-npm`, publishes the npm packages.

## One-time setup (an admin)

1. **GitHub environment.** In Settings → Environments → New environment, create `release`:
   - **Required reviewers:** the owner. With "Prevent self-review" on, someone other than the person who started the run must approve.
   - **Deployment branches and tags:** Selected branches → `main`.
   - No secrets or variables: the workflow needs none.

   Until this exists, every publish request fails in the build job.
2. **npm**, only before the first `github-and-npm` release:
   - **Scope.** The `@norn` scope must belong to an npm organisation the owner controls. On 2026-11-12, none of the four package names existed on the registry.
   - **Trusted publishing.** For each of `@norn/cli`, `@norn/authoring`, `@norn/dev-host-linux-x64` and `@norn/dev-host-linux-arm64`, add a trusted publisher in the package's settings → Trusted publishing → GitHub Actions, with:
     - organisation `Norn-cloud`;
     - repository `norn`;
     - workflow `release.yml`;
     - environment `release`.

     No npm token is created or stored anywhere.
   - **If npm only allows that setup on a package that already exists**, the first version of each package needs a one-time manual publish:
     1. Download the dry run's `release` artifact.
     2. Check its `SHA256SUMS`.
     3. From the owner's machine, run `npm publish <tarball> --tag alpha --access public`, with npm's two-factor prompt.
     4. Add the trusted publisher.

     Releases after that go through the workflow.

## Each release

1. **Bump the version.** Change `"version"` in `tools/norn-ts/package.json` to the next `0.1.0-alpha.N` in an ordinary pull request, and let it merge.
2. **Dry run.** Actions → Release → Run workflow. Branch `main`, publish `none`. The run summary lists the SHA256SUMS. A hosted runner may take up to two hours, the job's limit.
3. **Publish.** Run the workflow again with:
   - publish `github` for a GitHub pre-release only, or `github-and-npm` to also publish to npm;
   - confirm-version set to that exact version.
4. **Approve.** When the build job passes, the run shows "Review deployments". The reviewer opens it, selects `release` and chooses Approve and deploy. Rejecting or ignoring it publishes nothing; a pending deployment is rejected automatically after 30 days.

## What becomes public, and what is permanent

| What | Where | Can it be undone? |
|---|---|---|
| Tag `v<version>` at the commit | the repository | It can be deleted, but the released CLI fetches its dev host from that release's URL, so deleting it breaks every install of that version. Treat it as permanent. |
| The pre-release and its files: CLI and authoring tarballs, dev hosts, `manifest.json`, `SHA256SUMS` | GitHub Releases | It can be deleted (`gh release delete v<version> --cleanup-tag`), with the same breakage. |
| Build attestations: repository, workflow, commit, run, and each file's sha256 | the repository's attestations, and Sigstore's public transparency log | **Permanent.** The transparency-log entries cannot be removed. |
| npm packages under dist-tag `alpha` (`github-and-npm` only) | registry.npmjs.org | A version can be unpublished only within 72 hours and only if nothing depends on it. Its number can never be reused; after 72 hours it can only be deprecated. |

Nothing private is published. The tarballs hold only:
- files tracked at the commit (`assemble.py` proves it);
- the built files on its closed list;
- the third-party licence texts.
