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
| Last reviewed | 2026-10-31 |

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
- One line `steel-core` may print to stderr, once per process, when its home directory cannot be
  created on a fresh account.

## The rules that output follows

- A refusal says why in words and names what it is about: the capability, the argument, the
  bound and the size, the key, or the name the sandbox refused.
- A refusal the caller can act on says what to do: retry or raise the worker limit, put the
  definitions and the call in the same program, use one of the listed verbs.
- An error from a resource is not reworded: an uncaught one keeps its own type and text, and a
  program's own error keeps the program's words even when it quotes another's.
- No color, no terminal control codes and no markup are written by this crate.

## How it is tested

- `tests/sandbox.rs` (`refused`, and the tests that use it) pins `is not available to a program`
  and the name refused; `there_is_no_default_input_port` pins its message.
- `src/lib.rs`'s `a_definition_from_an_earlier_eval_reports_isolation_not_a_panic` and
  `an_identifier_never_defined_also_explains_the_isolation`, and `tests/isolation.rs`'s
  `a_name_from_an_earlier_eval_is_unbound`, pin the isolation hint and the name.
- `tests/bounds.rs` pins that each bound's refusal names the bound; `tests/worker_ceiling.rs`
  that the busy refusal names `Limits::workers`; `tests/minors.rs` the missing program, the
  undeclared verb and the 64-bit range; `tests/read_carryover.rs` the unterminated `read`;
  `tests/signed_run.rs` the rejected signature and the untrusted key.
- `tests/retype.rs` pins that an uncaught error from a resource keeps its type and text, and
  that a program's own error keeps its words.

Steel's own messages are not tested here. **No automated accessibility checker, browser or
assistive technology has been used on this output.**

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
tested, with what, and what was not.

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
