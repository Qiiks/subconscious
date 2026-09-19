# The contribution gate

Every repository we own runs the same contribution gate, so a contributor who
has learned the rule in one of them has learned it in all of them. The gate has
one canonical home — this repository — and every other repository calls it with
`uses:`. Nobody copies it; two copies drift.

> Behaviour changes need an approved issue first. Bypass is the
> maintainer-applied `trivial` label, full stop. A path glob is a judgement
> about effect made from a filename, and the filename is not where the effect
> lives — a test-only PR can delete the test that guarded a behaviour.

## What the gate does

A pull request must link an issue carrying a maintainer-applied
`design-approved` label. If it does not, the gate comments once, converts the
pull request back to draft, and publishes a failing `design-gate` check on the
head commit.

Applying `design-approved` to the issue later flips the waiting drafts to ready
automatically and turns their check green, with no push required.

Bypasses are a maintainer-only `trivial` label on the pull request, and the
`ci/**`, `train/**` and `alfonso/**` branch prefixes. The prefixes count only
when the head branch is in the repository itself, so a fork cannot buy a bypass
by naming its branch after a maintainer one. They are path prefixes rather than
substrings: a branch simply named `train` is an ordinary branch.

It runs on `pull_request_target`, so it has the base repository's context and
its secrets. Two tokens do the work. A CK CI App token posts the comment and
converts the pull request back to draft, which is what makes those writes work
on a fork pull request. The `design-gate` check run — the one output branch
protection actually reads — is published with the calling job's own
`GITHUB_TOKEN`, so the path that decides whether a pull request is blocked
depends on nothing but the workflow itself.

## What a repository adds

### 1. The caller workflow

`.github/workflows/design-gate.yml`, in the adopting repository:

```yaml
name: Design gate

on:
  pull_request_target:
    types: [opened, edited, reopened, synchronize, ready_for_review]
  issues:
    types: [labeled]

jobs:
  design-gate:
    permissions:
      contents: read # fetch the gate action
      issues: read # read the linked issue's labels
      pull-requests: write # gate comment, draft conversion, ready for review
      checks: write # publish the design-gate check run
    uses: cortexkit/subconscious/.github/workflows/design-gate.yml@master
    secrets:
      CK_CI_APP_ID: ${{ secrets.CK_CI_APP_ID }}
      CK_CI_APP_PRIVATE_KEY: ${{ secrets.CK_CI_APP_PRIVATE_KEY }}
```

The triggers live in the caller because a reusable workflow cannot add any.
Both trigger blocks are required: `pull_request_target` evaluates pull
requests, and `issues: labeled` is what releases the drafts waiting on an issue
when a maintainer approves it. A caller that declares only the first gets a
gate that blocks and never unblocks.

The `permissions:` block is an obligation rather than a knob, and `checks:
write` is the half worth understanding. The gate publishes the `design-gate`
check run with the calling job's own `GITHUB_TOKEN`, so that the blocking path
needs no permission from outside the workflow — but a called workflow can only
narrow the scopes its caller grants, never widen them, and a repository whose
default workflow permissions are read-only grants none of them by default.
Withhold `checks: write` and the publish is refused; the gate fails closed, so
every pull request stays blocked. Copy all four lines: naming any permission
sets every unnamed one to `none`.

Nothing else is configurable, and that is deliberate. The decision of which
event arm runs lives inside the reusable workflow rather than in an `if:` in
the caller, so a repository cannot switch the gate off by leaving a condition
out.

### 2. The two secrets

| Secret | What it is |
| --- | --- |
| `CK_CI_APP_ID` | App id of the CK CI GitHub App |
| `CK_CI_APP_PRIVATE_KEY` | That app's private key |

Both are declared `required: true` on `on.workflow_call.secrets`, so a caller
that forgets one fails when the workflow is parsed rather than part-way through
the run at the token mint step. That is why the template passes them explicitly
instead of using `secrets: inherit`.

The app needs issue read and pull request write on the adopting repository. It
does not need checks write: the check run is published with the caller's own
`GITHUB_TOKEN`, not with the app token. That split is deliberate. A run can
read the permissions a workflow grants — they are in the file above — but
nothing in a run can read an app installation's permissions back, because that
query needs the app's own credentials. Leaving the fail-closed publish on the
app token would have made every pull request's mergeability depend on a
setting nobody at the keyboard could check.

### 3. The two labels

| Label | Lives on | Applied by |
| --- | --- | --- |
| `design-approved` | issues | a maintainer, once the design is agreed |
| `trivial` | pull requests | a maintainer, when there is no design to agree |

Both must exist in the adopting repository before the gate is switched on.

Create them under the operator's `gh` as a one-time repository-admin act.
`gh label create` is not declared in the gh-shim manifest, so a seat's first
lift at this step hits a shim refusal; until shim v14 lands, that refusal is
the expected response and not a blocker — it is telling you the step is the
operator's, not the seat's.

### 4. The pull request template

Copy `.github/pull_request_template.md` from this repository. It carries the
`Closes #` line in the place a contributor actually reads, and the gate's
parser is pinned against it: `scripts/design-gate.test.mjs` asserts that an
unfilled template does not satisfy the gate and that filling in the number
does.

### 5. Branch protection

Make **`design-gate`** a required status check.

Do **not** require `design-gate / design-gate`. That name is real — GitHub
names a job called from a reusable workflow `<caller job id> / <called job
id>`, and the prefix cannot be suppressed because `uses:` takes no expressions
— but half of it is the calling repository's own job id. Requiring it would
write a name the repository can rename into that repository's branch-protection
contract.

`design-gate` is published on the head commit by the gate script itself, from
both event arms, so the name has exactly one author and no caller edit can
change it. The composed `design-gate / design-gate` job status is
informational: read it when you want to know whether the run itself failed.

## The trap

> A required check that a sha never produces blocks that sha forever. When a
> repo makes this gate a required status on a public branch, the job name is
> part of the contract and the triggers must be unfiltered — the first renamed
> job or paths-filter blocks main permanently, with nothing in the run list to
> say why.

Both halves of that bite, and they are answered differently.

The job-name half is answered by construction: the required check is published
by the script under a name the script owns, so renaming the caller's job
changes only the informational status. This is worth knowing rather than
trusting — it is the one part of the contract that a repository cannot break by
editing its own workflow, and the reason the reusable workflow contains no
checkout is the same reason: remove the argument rather than trust it.

The trigger half is not answered by construction and is entirely on the
adopting repository. **Do not add a `paths:` or `paths-ignore:` filter to the
caller's `on:` block, and do not narrow the `types:` lists.** A filtered
trigger means a docs-only pull request produces no run, no run produces no
`design-gate` check, and a required check that is never produced leaves the
pull request unmergeable with an empty check list. Copy the `on:` block above
as it stands.

The `types:` list matters for the same reason in the other direction: label
changes on a pull request do not appear in it, so applying `trivial` to an
already-failing pull request does not re-run the gate. Re-run it from the
checks tab, or push.

## Why there is no checkout

`pull_request_target` runs in the base repository's context with access to its
secrets, so contributor code from the pull request head must never be checked
out or executed. The reusable workflow therefore contains no `actions/checkout`
step and no `git` invocation at all. The gate script is packaged as a composite
action, `.github/actions/design-gate`, which the runner fetches itself — so
there is no ref for a caller to supply and nothing a caller can pass that
reaches a checkout.

That is stronger than checking out the base ref correctly, which is what the
gate did before: correctness by omitted argument survives only until someone
adds the argument back. `scripts/design-gate.test.mjs` asserts the absence
against both files that actually run the gate.

## Verifying a lift

```
node --test scripts/design-gate.test.mjs
```

**The suite is 87 arms.** That number is pinned here on purpose: a lift that
reports anything other than `# tests 87` has a setup defect before it has a
gate. The security arms read the workflow and the action from disk, and a
missing file reports as a named failure — `workflow file missing at <path>` —
rather than as arms that quietly fail to register.

`actionlint` covers the reusable workflow.

## Changing the gate

Changes land here and every caller picks them up, so a change to the gate is a
change to every repository we own. The callers pin `@master`; pin a tag instead
if a repository needs to hold a version, and note that the reusable workflow
references its composite action by an explicit ref of its own, so the two move
together only on `master`.
