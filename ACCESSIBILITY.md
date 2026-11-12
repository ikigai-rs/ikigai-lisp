# Accessibility

What `ikigai-lisp` emits that people read, the rules that output follows, how it is tested, and
how to report a barrier. It is a library: it evaluates a person's program and answers with the
program's value or a message about why it did not run, as text, and a host decides how a person
sees it. Many of those messages are Steel's, the Scheme this crate embeds, passed through; this
file says which. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only, no HTML or SVG |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-lisp/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-lisp/security/advisories/new) |
| Last reviewed | 2026-11-12 |

## What it emits that people read

- **A program's value** (`src/lib.rs`, `render_value`): the last value as `text/plain`, a string
  as its contents and anything else as Steel writes it (`3`, `#t`, `'(1 2)`).
- **This crate's own messages**:
  - refusals before a program runs (`src/lib.rs`): a missing capability (`urn:lisp:eval
    requires the urn:cap:lisp capability`), an unsupported `dialect`, a verb other than Source,
    a program or its data past a bound (`src/limits.rs`: its size or depth, the bound, and the
    host setting that holds it), all eval workers busy (`transient — retry, or raise …`), and a
    signed program whose key is not trusted or whose signature is rejected;
  - the sandbox's refusals (`src/sandbox.rs`): `` `name` is not available to a program ``, with
    why;
  - the hint added to Steel's unbound-name error (`ISOLATION_HINT`): each evaluation is
    isolated, so a definition from an earlier one is not in scope, and what to do instead;
  - the builtins' errors (`src/lib.rs`, and the Scheme prelude): an invalid IRI, a resource that
    answered bytes that are not UTF-8, an `(invoke …)` with an unknown verb or a dangling name, a
    value nested too deep or a number outside the 64-bit integer range, and `read`'s `input ends
    inside a form (unclosed paren or string)`;
  - `lisp: evaluator panicked: …` with the panic's own text.
- **The doors' self-descriptions**: the summaries of `urn:lisp:eval`, `urn:lisp:run`, a stored
  program's door and their arguments, which a host shows in its catalog, help or tool list.
- **The examples** (`examples/*.rs`) print timing measurements for developers, as plain lines.

Not this repository's, though it passes them on:

- **Steel's errors.** A program that does not compile or fails at run time answers `lisp: ` and
  then Steel's text, `Error: <Kind>: <message>`; the kind and the message are Steel's.
- **Errors from the resources a program reaches.** A program that catches one sees its text; one
  it does not catch leaves the evaluation as that same error, unchanged, so its words are the
  resource's.
- **A program's own `(error …)`**: its words are the program author's.

The fresh-account stderr warning previously listed here was addressed in
[PR 54](https://github.com/ikigai-rs/ikigai-lisp/pull/54). `tests/empty_home.rs` checks
that an evaluation is silent with and without `~/.local/share`.

## The rules that output follows

- A refusal says why in words and names what it is about: the capability, the argument, the
  bound and the size, the key, or the name the sandbox refused.
- A refusal the caller can act on says what to do: retry or raise the worker limit, put the
  definitions and the call in the same program, use one of the listed verbs.
- An error from a resource is not reworded: an uncaught one keeps its own type and text, and a
  program's own error keeps the program's words even when it quotes another's.
- This crate adds no color, terminal control codes or markup to its messages. A program
  or resource can supply its own text; a host must decide how to present it.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with evidence, following the
[closing-sweep example in PR 91](https://github.com/ikigai-rs/ikigai-gonk-starter/pull/91).
A Pass covers only the stated assertions, not overall accessibility or WCAG conformance.

The automated checks below were run on 2026-11-12 with `cargo test --workspace`, Cargo
1.99.1 and rustc 1.99.1, against main at
[`99b5d43`](https://github.com/ikigai-rs/ikigai-lisp/commit/99b5d43dc730f40dddb8058db43635ca71bc7879).
CI repeats that command through `.github/workflows/ci.yml`. These are Rust regression tests
of values, error types and selected message fragments. They do not evaluate how a host
presents text, whether people understand it, or every possible message.

| Check | Result | Evidence |
|---|---|---|
| Sandbox refusals name the refused operation | Pass | `tests/sandbox.rs`: `refused` asserts the name and `is not available to a program`; `there_is_no_default_input_port` checks its refusal text. Run with the command and versions above; not a usability test. |
| Isolation guidance | Pass | `src/lib.rs`: `a_definition_from_an_earlier_eval_reports_isolation_not_a_panic` and `an_identifier_never_defined_also_explains_the_isolation` check the hint; `tests/isolation.rs`: `a_name_from_an_earlier_eval_is_unbound` checks the name. Run with the command and versions above; not every Steel error. |
| Bounds identify the argument or setting | Pass | `tests/bounds.rs` asserts selected argument names and `Limits` settings in refusals. Run with the command and versions above; does not assess readability of all bound messages. |
| Busy-worker guidance | Pass | `tests/worker_ceiling.rs` asserts a transient refusal naming `Limits::workers`. Run with the command and versions above; does not assess whether the retry guidance is understood. |
| Self-call depth refusal | Pass | `tests/self_call.rs` checks permanent `DepthExceeded` rather than a busy refusal and verifies subsequent evaluation. Run with the command and versions above; checks error type, not wording. |
| Missing program, undeclared verb and integer range | Pass | `tests/minors.rs` checks missing `in`, rejected `verb` and the `i64` range fragment. Run with the command and versions above; does not evaluate full refusal sentences. |
| Unterminated read | Pass | `tests/read_carryover.rs` asserts `ends inside a form`, catchability and subsequent reads. Run with the command and versions above; not a user comprehension check. |
| Signature and trust refusals | Pass | `tests/signed_run.rs` asserts `signature rejected` and `trust set` with typed denials. Run with the command and versions above; does not check every word or key presentation. |
| Resource and program errors retain their meaning | Pass | `tests/retype.rs` checks exact type and text for uncaught resource failures and selected words for program-authored errors. Run with the command and versions above; not every possible error string. |
| Fresh-account stderr | Pass | `tests/empty_home.rs` checks empty stderr in child processes with and without a data home, and no writes in the missing-data-home case. Run with the command and versions above; does not cover every host environment. |
| Steel error clarity and source positions | Blocked | No current evaluation of Steel's own messages; the 2026-10-31 source review below is historical context, not a repeated CI check. Regression tests above cover selected fragments only. |
| Plain-text presentation, rendered values, door descriptions and example timing output | Blocked | No comprehensive accessibility evaluation of these surfaces or their presentation by a host was run. Source inspection and functional tests do not establish readable presentation. |
| Automated accessibility checker | Blocked | No axe or other accessibility checker was run; this crate emits text and no host page or browser checker was configured for this review. |
| Browser layout, zoom, contrast and reflow | Blocked | No host browser presentation was available for evaluation; the crate supplies no page to render. |
| Manual keyboard | Blocked | No person performed a keyboard pass in a host interface for this review. |
| Screen reader | Blocked | No screen reader session was available; no screen reader was used on this output. |
| Screen magnifier | Blocked | No magnifier session was available; no screen magnifier was used on this output. |
| Speech input | Blocked | No speech-input session was available; no speech-input software was used on this output. |
| Switch access | Blocked | No switch-access session was available; no switch-access software was used on this output. |
| Braille display | Blocked | No braille display was available or used on this output. |

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

- Steel's error text names no line or column of the program: Steel keeps the position, but the
  text this crate passes on (`Display`) leaves it out. The kind is written as a code identifier
  (`FreeIdentifier`, `TypeMismatch`, `ArityMismatch`).
- The sandbox's refusals point the reader at `src/sandbox.rs`, a source file in this repository
  that a person running a program may not have.
- A term `(graph …)` cannot convert is reported as `sparql-select: unsupported term in query
  s-expr`, the name of the other builtin that shares the converter.
- The verb refusal writes the verb as a code identifier (`Exists`, `Sink`).
- `lisp: evaluator panicked: …` passes on a Rust panic's text, which may mean nothing to the
  program's author.

## Contributing

Read this file before changing any output listed above: a refusal, a builtin's error, the
isolation hint, how a value is rendered or a door's description. Keep meaning in words, name
what a message is about, and keep another's words unchanged. Say in the pull request what was
tested in a `Check | Result | Evidence` table. Give each named check Pass, Fail or Blocked,
with the test or command and tool version, or why it could not run. Keep unrun checks as
Blocked, including assistive technologies nobody used. A historical run CI does not repeat
is Blocked with its date, not Pass.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-lisp/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and any assistive technology you use,
if you are comfortable sharing it. A barrier in Steel's own messages can be reported here too.
Anything security-sensitive goes through
[private vulnerability reporting](https://github.com/ikigai-rs/ikigai-lisp/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-12 | Record check results and scope, mark unrun evaluations Blocked, and remove the stale fresh-account stderr warning fixed in PR 54. |
