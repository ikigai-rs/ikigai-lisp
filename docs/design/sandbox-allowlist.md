# The sandbox as a constructive allowlist

Audit round 6 (ledger #903) reproduced five ways a program holding only `urn:cap:lisp`
reached the host: `(require "<path>")` loaded and ran any readable file (H-F2), `env-var`
and `command-line` read the host's environment and argv (C-R1, C-R2), and
`require-builtin` loaded `steel/process` and `steel/git` (C-R3, C-R4). Steel's
`Engine::new_sandboxed` blocks dylib loading and the direct file builtins and leaves
about 1,800 globals bound beside them.

The fix does not map what is dangerous. It builds the program-visible world up from what
is **needed** and refuses everything else, so a name nobody thought of is refused by
default, and a Steel upgrade that adds a global adds a refused one.

## What a program can reach

A program's world is four things, and each is closed separately:

| channel | how a program reaches it | how it is closed |
|---|---|---|
| global names | writing the name | rebound to a refusal unless on the allowlist |
| macros | writing a macro form | removed from scope unless on the macro list |
| modules | `require`, `require-builtin` | refused before compilation; a resolver backs it |
| code run at compile time | `defmacro`, `begin-for-syntax`, a procedural `define-syntax`; the constant folder | refused before compilation |

Special forms (`define`, `lambda`, `if`, `let`, `begin`, `set!`, `quote`, and
`define-syntax` with `syntax-rules`) are syntax, reach nothing, and stay.

## The mechanism

All of it is in `src/sandbox.rs`, run on each engine after its prelude (`lock_down`) and
on each program before it compiles (`screen`). Every claim below was read from Steel's
source (`steel-core` 0.8.3), not found by exercising the host surface.

1. **Rebinding a global after build takes effect.** A compiled closure reads a global by
   its slot index at call time (`CALLGLOBAL`), and `Engine::update_value` writes that
   slot. So rebinding `env-var` to a refusal reaches every caller, the stdlib's own code
   included, which is why the stdlib's internal needs have to be on the list (below).
   A refusal is a function that raises a catchable error naming itself; passing one
   around reaches nothing. Lock-down costs about 0.5 ms of the ~55 ms engine build, off
   the request path; `examples/eval_cost.rs` shows no measurable change.

2. **Macros** are a separate table (`in_scope_macros_mut`). Everything but the macro list
   is removed, so a removed macro reads as an ordinary call to a refused name.

3. **Modules.** `require-builtin` expands to `(define x (%module-get% %-builtin-module-M 'x))`,
   so refusing `%module-get%` and the module globals already closes it at run time.
   `register_module` overwrites a module by name and could have emptied the host modules;
   it is not needed, and the screen refuses the form before it compiles anyway, with a
   clearer error. For `require`, Steel resolves a string literal against its built-in
   source modules first, then against registered `SourceModuleResolver`s, and only then
   against the filesystem (`parse_require_object_inner`). A catch-all resolver that claims
   every path and resolves none therefore turns any path `require` into "unable to find
   module" without touching a file. Built-in source modules are resolved before
   resolvers, so the resolver cannot refuse those; the screen does.

4. **Code at expansion time.** The compiler holds a second Steel engine, its macro
   kernel, built with the full stdlib, and this crate has no handle on it. Three forms
   evaluate program code there: `defmacro` and `begin-for-syntax` (collected from the
   expanded top level, `Kernel::load_syntax_transformers`), and a `define-syntax` the
   compiler did not lower to `syntax-rules`, which the kernel's own `define-syntax`
   transformer `eval`s (`scheme/kernel.scm`). The screen parses the program exactly as
   the compiler does (`Parser::new(..).without_lowering()` then
   `lower_macro_and_require_definitions`) and refuses those identifiers, and the
   `define-syntax` keyword wherever lowering left it standing, ANYWHERE in the program:
   quoted, in a macro template, or as a macro argument, so a `syntax-rules` macro cannot
   assemble one. `syntax-rules` macros are pattern rewriting and stay.

5. **The constant folder** evaluates calls to Steel's pure primitives at compile time from
   its own table, keyed by their private `#%prim.` names, so `(#%prim.string-ref "abc" 0)`
   folded even with `string-ref` refused. Programs may not write Steel's private names
   (`#%…`, `##…`) at all; the reader's own spellings of quote and unquote are the
   exception. A program never needs one: the allowed macros expand INTO some, after the
   screen has run.

6. **The stdio ports** are captured inside stdlib parameters when the engine is built, so
   removing the native port constructors does not cut them. They are not replaced: they
   are made unreachable. No program needs an output function, so `display`, `write`,
   `newline` and the port parameters are all refused names, and the prelude's `read`
   takes a string port as an argument instead of falling back to `current-input-port`
   (the host's stdin, which an MCP stdio host carries its protocol on). The pin below
   proves nothing on the allowlist reaches a port parameter, because no port parameter is
   on it.

## The lists

**This crate's surface** (`PRELUDE_NAMES`): the kernel verbs and their friends, the
primitives under them, and `read` with the four reader primitives it uses. All defined by
this crate.

**What programs need** (`PROGRAM_NAMES`): every Steel global the known programs reference
after macro expansion — the ikigai-programs corpus (`programs/*.scm`), this crate's tests
and README, the runbook's `urn:lisp:eval` examples, and the alias prelude a host generates
(`urn:lisp:aliases`). Numbers, lists, strings and characters, equality, `open-input-string`,
and what the allowed macros expand into. Macros: `and`, `or`, `cond`, `let*`,
`quasiquote`, `with-handler`, and the `reset`/`shift` that `with-handler` expands into.

**What the stdlib needs internally** (`STDLIB_INTERNALS`): DERIVED. The closure of the two
lists above over the reference graph of Steel's compiled stdlib, minus the lists
themselves: the `#%prim.` primitives the stdlib's definitions call, thread-local storage
and the continuation machinery behind `with-handler`, the module definitions behind `map`,
`filter`, `assoc` and the `c…r`s, and what the prelude's `read` uses. 34 entries; nothing
in it reaches the host.

**Lifted functions** are the one class. The compiler lifts a lambda that captures nothing
into a global named `##__lifted_pure_function<parse counter>`, so the names cannot be
listed. A capture-free closure holds no authority of its own (it reaches globals, which are
allowed or refused like any other), so all of them stay, and the pin asserts each is a
capture-free closure.

### How the derived list was derived

Reading, not running: `derive::graph` walks the expanded AST of every compiled stdlib
module (`Engine::modules`) and of the top-level programs the build runs (Steel's
`stdlib.scm`, then this crate's prelude), recording for each definition the names its body
mentions. Two refinements keep it honest rather than merely safe: a module import
(`(%proto-hash-get% __module-P 'x)`) is a reference to `P`'s `x`, resolved when the module
loaded, not to the whole export table; and the LAST top-level definer of a name wins, as at
run time (the prelude's `read` replaces Steel's, so Steel's reader is not needed). Locals
that share a global's name are counted, so the closure over-approximates, never under.

The program list is the expansion of the programs themselves. To regenerate both, put the
programs one per file in a directory and run:

```text
IKIGAI_LISP_DERIVE=<dir> cargo test --lib derive_from_programs -- --ignored --nocapture
```

It prints the globals the programs reference (SEEDS), the closure beyond them (INTERNAL),
and the macros they use. On 2026-10-09 the corpus, the crate's tests and README, the
runbook examples and the alias shape gave 72 seeds. Six of them (`make-struct-type` and
what a `struct` form expands into) came only from one isolation test that defines a
`struct` to prove it does not leak; no program defines one, so `struct` stays refused and
that test now asserts the refusal. One more, `compose`, is a name the corpus DEFINES (a
program's own definition overwrites a refused global, in its own engine), so it is not on
the list. The result: 34 + 54 + 34 names and 8 macros.

## The pins

In `src/sandbox.rs`:

- `the_names_a_program_can_reach_equal_the_allowlist` builds an engine, records every
  global's value, locks it down, and calls a name reachable when its value was left
  alone. The reachable set must EQUAL the allowlist: not "contains no known bad name".
  A global a Steel upgrade adds is refused, so it does not change this set; an allowlist
  entry that stops resolving, or a value the allowlist did not intend to keep, does.
- `the_internals_are_the_closure_of_the_program_names` recomputes the derived list and
  fails, saying which way, when Steel's internals move.
- `only_the_allowed_macros_are_in_scope`, `every_allowlist_entry_resolves`, and the
  screen's own tests.

`tests/sandbox.rs` holds the regression tests for H-F2 and C-R1–R4. Each takes the audit
reproduction's input and asserts the refusal, a typed `Denied`; none reaches the host.

## What changes for a program

Anything outside the lists is refused, including names a REPL user may reach for: output
(`display`), `vector-ref`, `string-join`, `when`/`unless`, `struct`. Adding a pure function
or a macro is a one-line change to a list, and the pins say whether the stdlib needs
anything more for it. `(read)` with no port is an error that says to read from a string
port. The ikigai-programs corpus runs unchanged (177 tests).

## Not covered here

- **Which Steel.** The lists are verified against the locked `steel-core` (0.8.3). The
  manifest's `"0.8.2"` lets a consumer resolve another 0.8.x; there the sandbox fails
  CLOSED (an internal it does not list is refused, so a stdlib function breaks), never
  open. Pinning Steel exactly would make "every Steel this runs on was pinned by CI" true
  for consumers too; that is a manifest policy decision, not made here.
- **Steel's own kernel transformers** (`struct`, `define-values`) still run in the macro
  kernel on program syntax. They are Steel's code treating that syntax as data; none
  evaluates it.
- **Bounds.** What an allowed function may allocate, how deep a program or a datum may
  nest, and how long a runaway may run are steps 3 and 4 of #903, not this one.
