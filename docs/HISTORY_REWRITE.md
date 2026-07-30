# History rewrite — 2026-07-30

FamilyClaw's git history was rewritten once, immediately before the v1.3.0
Public Preview, to make the repository publishable. This document records
exactly what changed, so that anyone comparing an old local clone against the
published one can see why the commit IDs differ.

**If you have a clone from before 2026-07-30, its commit IDs no longer exist.**
Re-clone. Do not merge an old clone into the new history; it will resurrect
every rewritten commit.

## Why

Three categories of private material shipped with the repository and could not
be removed by editing files:

1. **Commit author and committer identity.** Several commits were authored by
   private agent identities and by a local `root@<machine-hostname>` account.
   This metadata appears in no diff and in no commit message, so both leak
   gates reported clean. It would have been published on every commit page and
   in the contributors graph on day one. Neither gate checked `%an/%ae/%cn/%ce`
   at all — that blind spot is now closed by `audit-layer-b.sh` check #12 and
   `pre-publish-scan.sh` check #2b.
2. **Private names and a personal email address** in historical file contents
   and commit messages. The working tree had been cleaned; history had not.
3. **A throwaway RSA test key** that lived in
   `crates/familyclaw-gateway/src/oidc.rs` for two commits before being
   replaced by runtime key generation. It never protected anything and needs no
   rotation, but a literal PEM-format RSA private-key header block in a public
   history forces every reader and every scanner to stop and prove it was
   harmless.

## What was rewritten

Tool: `git-filter-repo`, run against a fresh single-branch clone. The original
repository was not modified.

**Scope:** all 366 commits of the canonical release branch. Old tags were
deliberately **not** carried over — they point at commits that no longer exist
and whose metadata is precisely what this rewrite removed.

### Identity metadata (`--mailmap`)

| Original identity | Rewritten to | Commits |
|---|---|---|
| three private agent identities (`*@familyclaw.local`, `*@familyclaw.dev`) | `FamilyClaw Contributor <noreply@users.noreply.github.com>` | 59 |
| `root@<build-machine-hostname>` | `FamilyClaw Contributor <noreply@users.noreply.github.com>` | 14 |
| maintainer's **personal** email address | `Sisuthros <219464239+Sisuthros@users.noreply.github.com>` | 51 |

Author, author-email, committer and committer-email were all rewritten.

The maintainer's own commits were **not** flattened into the anonymous
contributor. He already authored 241 commits under his public GitHub identity;
collapsing genuine attribution into "FamilyClaw Contributor" would have
misrepresented authorship to remove an address that has a public equivalent.
Only the private address was folded into the public one.

**Result:** 4 distinct author/committer pairs remain, all public identities.
0 private identities, 0 personal email addresses.

### Text content and commit messages (`--replace-text`, `--replace-message`)

Private agent names → generic sample identifiers (`agent_alpha` …
`agent_epsilon`); operator names → `the operator` / `operator`; the full
maintainer name in copyright position → `The FamilyClaw Authors`; the personal
email address → the GitHub noreply address; the build-machine hostname →
`build-host`; the RSA test-key block → a one-line note pointing at the runtime
generation that replaced it.

The rules are **suffix-tolerant** (`name[a-zäöåA-ZÄÖÅ]*`). A first pass using
word-boundary rules missed 14 surviving forms, because Finnish attaches case
endings directly to the stem and one occurrence sat inside an
underscore-prefixed filename. One rule keeps its *leading* boundary, so that
ordinary Finnish words which merely end in the same letters are left alone.

## What was verified afterwards

- **The published tree is byte-identical to the audited tree.** The rewrite
  changed history only: `HEAD^{tree}` before the rewrite and after the rewrite
  are the same object. This was checked on every pass, and it caught two real
  mistakes — a `#`-prefixed line in a `--replace-text` file is a literal
  pattern, not a comment, and the first attempt silently corrupted 200+ files
  before the tree comparison exposed it.
- `scripts/audit-layer-b.sh` with the real deny list: **PASS**, 366 commits
  scanned for identity metadata.
- `scripts/pre-publish-scan.sh` with the real deny list: **PASS** — tree,
  commit messages, diff content, identity metadata, and history-wide secret
  patterns.
- Full `cargo fmt` / `clippy -D warnings` / `test --workspace --all-features`
  re-run after the rewrite, plus the crash proof from a fresh clone of the
  rewritten branch.

## Open item for the maintainer

The replacement identity `FamilyClaw Contributor
<noreply@users.noreply.github.com>` is a neutral placeholder chosen so the
rewrite could proceed. If a different attribution is preferred, the rewrite is
re-runnable from the recorded mailmap in a few seconds — but it must happen
**before** the history is pushed anywhere public, because after that the old
IDs are permanent for anyone who cloned.
