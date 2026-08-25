# Declaring external effects and a revert plan in the module contract

Proposal for the GRACE team. **v0.3, 2026-08-25.**
Russian original: `grace-proposal-revertible-effects.md`.

---

## Summary

1. Raise the existing **`SIDE_EFFECTS`** field from function level to module
   level: what the module changes outside its own boundary.
2. Add **`REVERT_PLAN`**: how to undo it.
3. Make `SIDE_EFFECTS` a required contract field; make `REVERT_PLAN` required
   conditionally, via a separate rule.

No new concepts: GRACE already has a field for external effects.

## Why: the argument comes from your own code

`skills/grace/grace-explainer/references/semantic-markup.md` documents a
function-contract field:

```
SIDE_EFFECTS: [What external state it modifies]
```

The same file explicitly forbids renaming it. So **the need to declare external
effects is already acknowledged in GRACE.**

However, function contracts are parsed but their fields are validated nowhere:
only four module fields are required (`PURPOSE`, `SCOPE`, `DEPENDS`, `LINKS`).
A field about external effects therefore **exists and is not checked** — exactly
the state in which a declaration drifts away from the code and nobody notices.

This proposal does not invent a field. It finishes one that is already there:
raise it to module level (an effect belongs to a module, not to a single
function), add the other half (how to undo it), and turn checking on.

## The gap in the model: the time axis

`DEPENDS` plus the graph under `.grace/graph/` describe **who depends on whom**.
Nothing describes **what stays in the system when a module is removed**.

Consequence: the obligation to clean up has nowhere to be declared, so nobody
fulfils it. This is not carelessness — each effect is written deliberately. It
is lost at removal time, where nobody remembers it any more.

## What counts as an external effect

A definition is needed, otherwise the field is unfalsifiable: a reviewer cannot
say "this is wrong", only "this feels wrong".

> An **external effect** is a state change that (a) outlives our process and
> (b) is observable by someone who has not read our code.

Clarifications:

- **reading is not an effect**;
- a resource created by another module **of the same project** is foreign to us:
  only the owning module declares it in `SIDE_EFFECTS`; others refer to the owner.

### A three-question test

1. **Does a trace remain?** After our process exits and our files are deleted,
   is anything still changed that an outsider can see? No → `none`.
2. **Can we restore the previous value ourselves?** Do we hold the previous
   state, and does it survive a repeated run? No → the effect is
   **irreversible**, and that must be stated plainly.
3. **Who owns the target?** Only us / another program / a shared system slot
   (port, global hotkey, clipboard) / a remote side.

## Effect classes

| Class | What it is | Form of undo |
|---|---|---|
| `own` | created entirely by us, ours to keep | delete |
| `foreign` | an edit inside a file owned by someone else | restore exactly our part |
| `process` | started by us, outlives us | stop |
| `other` | fits none of the above | describe in words |

`foreign` is the dangerous one: here "delete the file" is not an undo, it is a
new breakage.

`other` is a required safety valve. Two `other` cases in one project are a
reason to introduce a class, not to grow prose. On the pilot exactly one such
case appeared: the clipboard — a shared temporary slot where undo means
"restore the previous contents", not "delete".

If four classes prove too few, the reasonable next step is to classify by
**form of undo** rather than by kind of thing (created / modified / occupied /
started / remote / irreversible). That redesign is deliberately **not** part of
this proposal: first check whether four suffice.

## Format: strictly one line per field

This is a parser requirement, not a style preference. In
`src/project-utils.ts:497`:

```ts
const match = stripCommentPrefix(line).trim().match(/^([A-Z_]+):\s*(.*)$/);
```

A field is opened only by a line starting with Latin capitals and a colon.
**Anything written as a continuation is silently discarded.**

Verified on the pilot: in one contract five of eight lines were dropped,
including the only mention of an obligation to remove hooks from third-party
config files. A field created so that an obligation would not be lost was
losing it itself.

Two consequences:

- the proposed format is **one line per field**, classes separated by semicolons;
- this is not specific to the new fields. On the same project, continuations of
  existing `NOTE` and `SCOPE` fields are dropped the same way. The defect is
  general, and it surfaced only once field contents began to be read by machine.

Examples:

```
//   SIDE_EFFECTS: own: creates %APPDATA%\app\signals\ and deletes *.signal from it; foreign: the *.busy files are written by external hooks, ownership is shared
//   REVERT_PLAN: delete the signals\ directory; separately remove hook blocks from ~/.claude/settings.json, otherwise they call scripts that no longer exist
```

```
//   SIDE_EFFECTS: foreign: the "window.title" key in the editor's settings.json; a backup settings.json.app-bak is placed next to it
//   REVERT_PLAN: restore window.title from the backup; without a backup delete ONLY our key and do not rewrite the file
```

```
//   SIDE_EFFECTS: none
```

### The value "cannot be undone"

Without it, the rule "declared an effect, declare an undo" forces authors to
write falsehoods. A pilot example: the backup of the editor settings is
overwritten on every write, so from the second call the original value is
physically gone. Under lint pressure an author writes "restore from backup" —
and an agent will execute it.

> Accepted value: `REVERT_PLAN: irreversible — <reason>; compensation: <what we do instead>`.
> Lint accepts it but raises a warning, so irreversible effects appear as a list
> rather than dissolving into prose.

## Where it plugs in

**Only `SIDE_EFFECTS` becomes required:**

```ts
for (const field of ["PURPOSE", "SCOPE", "DEPENDS", "LINKS", "SIDE_EFFECTS"]) {
```

`REVERT_PLAN` is checked by a separate rule:

```
markup.effects-without-revert-plan —
  SIDE_EFFECTS normalises to something other than "none" while REVERT_PLAN is empty
```

Why this split: making both fields required would break the promise that "for
pure logic one line `none` is enough", and it would also make the separate rule
dead code, since it could never be reached. Version 0.2 of this proposal
contained both mistakes.

**Normalisation.** A field counts as empty only if the whole value, after
trimming and lowercasing, equals `none`. Dashes inside the value do not matter.
`REVERT_PLAN` counts as filled if it is non-empty and not one of `none`, `—`,
`tbd`, `todo`.

**What else has to change** (v0.2 claimed "nothing else in the linter", which is
wrong): the error-message catalogue, CLI documentation, tests, contract
templates and examples, the reviewer sub-agent prompt, and the GRACE 3 → 4
migrator, which needs something to put into a newly required field.

## What we observed, on one project, in one day

Pilot: a Rust + Win32 panel, 23 modules. The author of the proposal is also the
author of the code and the judge in unclear cases. That matters when reading
what follows.

**1. Filling the field exposed three unrecorded obligations.** A log with no
rotation; a backup of third-party settings that overwrites itself from the
second call; no uninstaller for hooks, so after the program is removed foreign
config files keep calling scripts that no longer exist.
*Shows*: a one-off sweep across modules pays for itself.
*Does not show*: that a permanent field pays for itself — a one-off audit would
have produced the same result at a one-off cost.

**2. Unchecked markup degrades.** Migrating the project to GRACE 4 surfaced 20
markup errors at once. Both numbers — 20 errors, and `lint = 0` after the fix —
were produced by your CLI, not by the author.
*Shows*: without checking, markup drifts away from code.
*Does not show*: that this particular field degrades.
*Competing explanation*, stated honestly: the project was on GRACE 3 and the
linter refused to validate it, so the check never existed in the first place —
it did not rot. A project without a linter says little about the fate of a
convention with one.

**3. The format loses data silently.** Continuation lines were discarded by the
parser — both in the new fields and in existing `NOTE` and `SCOPE` fields. It
was noticed only when field contents began to be read by machine.
*Shows*: what is not checked breaks quietly, with no human involved at all.

### What we did not observe

Not a single field filled in by anyone other than the proposal's author. Not a
single review cycle with an outside reviewer. Not a single revert plan actually
executed. Not a single case where the classification failed — which says more
about the narrowness of the pilot than about the completeness of the classes.

### A one-hour check that does not require trusting us

Take one of your own projects and write out the external effects module by
module. That is one to two hours. If not a single unrecorded obligation turns
up, the field does not pay for itself in your context and the proposal should be
rejected.

## The risk this proposal introduces

The advertised benefit — "an agent reads the revert plan and executes it" — is
also a new failure mode: **a stale plan means an agent performing a wrong
deletion inside someone else's file**. An automated destructive action based on
prose that nobody has reconciled with the code.

Mitigations worth writing into the rule from the start:

- an agent first prints the list of what it will touch, and only then touches it;
- for the `foreign` class and for irreversible effects, automatic execution is
  disabled by default.

A second risk, stated plainly: a newly required field breaks lint for every
existing project. The reasonable path is a warning first, an error from the next
major version.

A third: the recurring cost sits on **review**, not on writing. To confirm that
`none` is true, a reviewer has to read the module. That is per module, forever.

## What is not being claimed

The idea is borrowed from work on composition in time and space
([Cordis](https://github.com/cordiverse/paper), August 2026), which DeepSeek
Harness is built on. There, the runtime itself carries the inverse operation.

Here it is **a declared obligation, executed by a human or an agent — not a
runtime-guaranteed inverse**. Three properties of the original are specifically
not claimed:

1. **guaranteed execution** — a plan may fail;
2. **composition** — reverts do not compose automatically; ordering must be
   stated separately (a sensible default is the reverse of `DEPENDS`);
3. **totality** — not every effect has an undo; some are irreversible.

Hence `REVERT_PLAN` rather than `REVERT`: the suffix says this is a plan, not an
executable command. In v0.2 the field was called `REVERT` and the title said
"revertible effects" — the name promised a guarantee that does not exist. Names
travel through diffs; caveats in sections do not.

## Limitations

- The check catches **emptiness**, not **truth**: a declared effect drifting
  from the code is not syntactically detectable. This is a deliberate first
  step, not a solution.
- A prose plan is not executed automatically. A machine-readable form (a list of
  paths and commands) is possible future work, not part of this proposal.
- A plan must be safe to run repeatedly and when the target is absent: "nothing
  to undo" is success, not an error.
- The mechanism does not reach files outside the contract surface. On the pilot,
  the most dangerous effect — editing third-party agent configs — is performed
  by helper scripts outside `src`, which have no module contract. Contract
  fields **do not cover** that case; it needs a separate answer.
- Only two fields and one rule are being proposed. Everything else is an idea
  without commitment. The author is willing to do the first iteration.

## What already exists

Project `Baho73/claudebar` (23 modules, Rust + Win32), commits `250b2dd`,
`70719a6`, `3ea356a`:

- an inventory of external effects across all modules — `docs/effects-inventory.md`;
- fields filled in for eight modules (the pilot used the names
  `EFFECTS`/`REVERT`; this proposal renames them to
  `SIDE_EFFECTS`/`REVERT_PLAN`);
- the project migrated to GRACE 4: `grace lint` reports 0 errors, `grace status`
  reports `projectKind=grace4` with 0 integrity errors.
