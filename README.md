# ikigai-lisp

A [Steel](https://github.com/mattwparas/steel)-backed Lisp evaluator as an
[ikigai](https://github.com/ikigai-rs) module. One endpoint, `urn:lisp:eval`,
runs an s-expression whose builtins **are the kernel verbs** — `source`, `sink`,
`meta`, `exists`, `delete` — each issued back through the host kernel under the
eval's own capability. Code is a resource; the capability is the builtin set.

```text
source urn:lisp:eval '(source "urn:fn:toUpper" "hi")'   # -> HI
source urn:lisp:eval '(+ 1 2)'                           # -> 3
source urn:lisp:eval '(map (lambda (w) (source "urn:fn:toUpper" w)) (list "a" "b"))'
```

## Capability model

Two layers, both required:

1. **`urn:cap:lisp`** — gates "may run arbitrary Lisp at all," declared on the
   eval action's `requires`, so the kernel enforces it before dispatch (declared
   = enforced). The endpoint re-checks it at entry as a second line, for the
   paths where no kernel gate ran.
2. **Per-verb enforcement** — every verb sub-request carries the eval's
   capability, so a `(sink …)` the capability doesn't authorize comes back as a
   typed `Denied`, surfaced to the program as a catchable Steel error
   (`with-handler`) — never a panic. Left uncaught, it leaves the eval as that
   same typed `Denied` (and a sub-request's `Unavailable`/`Timeout` stays
   transient), so a caller, a Retry overlay or an agent sees the resource's
   refusal rather than an opaque endpoint string.

Both layers rest on the sandbox below: the verbs are the ONLY way out.

## The sandbox: an allowlist

A program holding only `urn:cap:lisp` reaches the world through the kernel verbs and
nothing else. Steel's own "sandboxed" engine leaves about 1,800 globals bound — the
environment, argv, process and git modules, runtime `eval`, the host's stdio ports —
and `require` loaded any file the service user could read (audit round 6, ledger
#903). So this crate does not try to name what is dangerous. It builds the
program-visible world from what programs NEED and refuses everything else:

- **Names.** After each engine is built, every global not on the allowlist is rebound
  to a refusal: the kernel verbs and this crate's prelude, the pure Steel functions the
  known programs use (numbers, lists, strings, `read` on a string port), and what those
  need internally — the last list DERIVED from the reference graph of Steel's compiled
  stdlib, never by trying things against the host. A Steel upgrade that adds a global
  adds a refused one.
- **Macros.** Only `and`, `or`, `cond`, `let*`, `quasiquote` and `with-handler` (with
  the `reset`/`shift` it expands into) stay in scope.
- **Forms that leave the evaluation** are refused before compiling: `require` of any
  path, `require-builtin`, and the three ways a program runs its own code at EXPANSION
  time in the compiler's macro engine (`defmacro`, `begin-for-syntax`, a `define-syntax`
  that is not `syntax-rules`); and Steel's private `#%…` names.
- There is **no default input port**: `read` reads a string port it is handed, so the
  host's stdin (an MCP stdio host's protocol) is out of reach, and so is its stdout.

A refusal is a catchable error inside the program and a typed `Denied` when left
uncaught. The pins: the names a program can reach after build EQUAL the allowlist (a
test observes every global before and after lock-down), and the derived list equals
the closure of the program list over THIS Steel's stdlib, so an upgrade that moves
Steel's internals fails CI saying which way. Anything off the list is refused —
`display`, `vector-ref`, `string-join`, `when`, `struct` — and adding a pure function or
macro is a one-line change the pins then check. The lists, how they were derived and
how to regenerate them: `src/sandbox.rs` and `docs/design/sandbox-allowlist.md`.

## Performance

Every eval runs on a **sandboxed Steel engine built for it alone**, so nothing one
evaluation defines — a global, a macro, a symbol's binding — is visible to another.
(A per-eval clone of one warm engine shared the compiler between clones, so a
`define-syntax` from one caller rewrote the next caller's program; ledger #903.)

Each pooled worker builds its next engine right after answering, while it would
otherwise be idle, so the build stays off the request path for ordinary traffic.
Measured with `cargo run --release --example eval_cost` (Apple silicon):

| traffic                       | warm clone (before) | engine per eval (now) |
|-------------------------------|---------------------|-----------------------|
| paced (a pause between evals) | 0.53 ms             | 0.37 ms               |
| back-to-back on one worker    | 0.29 ms             | 63 ms                 |
| first eval on a new worker    | 87 ms               | 77 ms                 |

A worker kept continuously busy is bounded by the build: about 60 ms of CPU per
eval. Concurrent callers spread across the pool, each worker building in parallel.
Re-measured after the sandbox landed (same machine, main against the branch): back to
back 56.3 against 55.3 ms, paced 0.76–0.81 against 0.77 ms — locking an engine down
costs about 0.5 ms of its build, off the request path, and the pre-compile screen about
2 µs for `(+ 1 2)` (0.9 ms for the largest corpus program, 106 KB, whose own compile and
run takes 27 ms).

## Stopping a runaway

The crate does not time evaluations itself: a host puts a wall-clock governor in front
of the space (`ikigai-throttle`'s `Timeout`). When it fires it drops the eval's future,
and that drop **interrupts the program**: Steel checks for an interrupt before every
instruction it dispatches and the flag stays set, so a `with-handler` handler cannot
catch its way past it. The worker is released, so a ceiling's worth of runaways no
longer takes `urn:lisp:eval` and `urn:lisp:run` down. Two limits: a verb PARKED on a
sub-request is released when the endpoint serving it yields — promptly for one that
awaits, only when it returns for one that blocks its thread (govern a blocking
dependency with its own overlay) — and nothing bounds memory in SIZE: no allowed
function takes a size, but a string doubled in a loop grows until the governor stops
it, and a binding with no governor has no bound at all.

## Bounds, and how a host sets them

Steel's reader, expander and compiler recurse once per level of nesting, and a stack
overflow aborts the host process rather than failing one eval. So nesting is bounded
before Steel sees it (program text, data a program `read`s, a value passed to
`(graph …)`/`(sparql-select …)`), each worker gets an explicit stack sized for the
deepest input the bounds admit, and program and data sizes are bounded too. Each
refusal is a typed error: `InvalidArgument` naming the argument at the door, a
catchable error inside the program.

| bound                | default                    | what it bounds |
|----------------------|----------------------------|----------------|
| `workers`            | available parallelism, ≥ 8 | live eval worker threads; past it, a transient `Unavailable` |
| `worker_stack_bytes` | 128 MiB (reserved)         | each worker's stack; the deepest admitted program needs 32 MiB with Steel unoptimized |
| `max_program_bytes`  | 4 MiB                      | program text |
| `max_input_bytes`    | 16 MiB                     | `(input)` data |
| `max_nesting`        | 1,000                      | levels of nesting in program text, `read` data, and values crossing into the s-expression compilers |

A host sets them once, before the first eval, from its **config home or its flags —
never environment variables** (`IKIGAI_LISP_WORKERS` is gone, ledger #214):

```rust,ignore
ikigai_lisp::set_limits(
    ikigai_lisp::Limits::default().workers(4).max_input_bytes(1 << 20),
).expect("before the first eval");
```

### Strings are UTF-8 — never index-scan one

Steel strings are Rust `String`s, so a *character* index costs a walk from the
start: `string-ref` is O(i) and `string-length` is O(n). The ordinary
`for i in 0..len` scan is therefore **quadratic**, and `(substring s i n)` inside a
loop is quadratic *and* allocating. Nothing about that code looks slow, which is
exactly the problem — one pass over a string of n characters, measured in release:

| n       | `string->list` then walk | `string-ref` per index | `string-length` in the loop test | `substring` per step |
|---------|--------------------------|------------------------|----------------------------------|----------------------|
| 25 000  | 3.5 ms                   | 15 ms                  | 25 ms                            | 0.28 s               |
| 50 000  | 7.2 ms                   | 51 ms                  | 98 ms                            | 1.04 s               |
| 100 000 | 11 ms                    | 151 ms                 | 321 ms                           | 4.11 s               |

Convert once with `string->list` and walk the chars; hoist `string-length` out of
any loop that tests it; and bound untrusted text with `utf8-length` (bytes, O(1))
*before* scanning it.

### `read` keeps a reader per port

Steel 0.8.2's `read` keeps one reader in a shared object and, on a string or file
port whose text does not close, returns eof while **leaving the partial form in
it** — so every later `read` appends its port to the stale fragment and reports
`(eof)`. This crate replaces `read` with one that keeps a reader per port for the
life of the evaluation: a port read for the first time gets a fresh reader, reads of
one port walk its datums in order however reads of other ports interleave with them,
and input that ends inside a form raises a catchable error naming the cause instead of
quietly reading as eof. It reads a string port it is handed — `(read (open-input-string
(input)))` — and nothing else.

## Opt-in caching

An eval is uncacheable by default (it may `sink`/mutate). A program can opt in:

```text
(cacheable (+ 1 2))                     ; permanently cacheable
(cacheable/ttl 300 (source "urn:x"))    ; cacheable for 300s
```

The opt-in is **ignored if the eval mutated** (a `sink`/`delete` forces
uncacheable), and the result is never fresher than its inputs — the kernel folds
the sourced resources' expiries and golden threads onto it, so cutting a sourced
resource's thread invalidates the cached eval automatically.

## Homoiconic SPARQL

`(sparql-select …)` compiles a query written **as data** (via
[`ikigai-sexpr`](https://github.com/ikigai-rs/ikigai-sexpr)) and runs it through
`urn:sparql:select` — compose queries with quasiquote, no string-building:

```text
(sparql-select '(select (?s ?p ?o) (where (?s ?p ?o)) (limit 3)))
(sparql-select `(select (?name) (where (?s ,pred ?name))))   ; splice in a predicate
```

## Reaching every endpoint — `(invoke …)`

The `(source iri [input])` / `(sink iri content)` wrappers carry the one
conventional argument each verb usually needs. `(invoke …)` is the **general
verb**: it takes a verb, an IRI, and trailing `"name" value` pairs, so a program
can drive *any* action's named arguments — not just the single-input ones:

```text
(invoke 'source "urn:sign:sign" "in" msg "key" "urn:secret:signing-key")
(invoke 'source "urn:secret:generate" "into" "signing-key" "type" "ed25519")
```

Names may be strings or symbols (`'in`); the verb is carried through, so an
`(invoke 'sink …)` / `(invoke 'delete …)` still forbids caching. A dangling name
or an unknown verb is a catchable error, never a panic.

## Homoiconic graphs — `(graph …)`

`(graph GRAPH-SEXPR)` is the graph-authoring dual of `(sparql-select …)`: it
compiles a `(graph …)` s-expression to canonical RDF **Turtle** (via
[`ikigai-sexpr`](https://github.com/ikigai-rs/ikigai-sexpr)'s `sexpr_to_turtle` —
the same compiler `urn:rdf:from-sexpr` uses, so the datum is portable). It is a
pure transform; the returned Turtle is then `sink`-able, signable, or diffable:

```text
(graph '(graph
  (prefix (ex "http://example.org/") (foaf "http://xmlns.com/foaf/0.1/"))
  (ex:alice a foaf:Person)
  (ex:alice foaf:name "Alice")))

(graph `(graph ,prefixes ,@triples))   ; composed with quasiquote, never string-built
```

Composed with the verbs, a single program can mint a key, author a graph, sign
it, and verify it — all homoiconically.

## Using it from a host

```rust,ignore
let space = ikigai_lisp::space(); // binds urn:lisp:eval
// mount into your kernel alongside the other modules, behind a Timeout overlay
```

The space names itself `urn:iki:space:lisp` (`ikigai_lisp::SPACE_ID`).

Set the bounds first if the defaults do not fit (above), and put a `Timeout` in front
of every binding — the embedded one too: it is what stops a runaway.

### Serving `urn:lisp:run`

`ikigai_lisp::run_signed(keys)` runs a program a trusted key signed, under the
submitter's capability, in the same sandbox and bounds. Three things a host serving it
should know:

- **A signed program replays.** The signature has no nonce, expiry or audience: anyone
  holding `urn:cap:lisp:run` who has seen a signed program can run it again, on any host
  trusting that key, for as long as the key is trusted. Sign programs that are safe to
  run more than once, or retire the key.
- **Its data is unsigned.** The submitter chooses the program's `(input)`; a program
  that acts on its input must treat it as the submitter's, not the signer's.
- **Trust is anchored on the key's IRI.** The key resolves through the kernel at verify
  time, so whoever can write that resource can swap the key. Bind it from a resource
  only the host controls.

Every door serves Source (and Meta, from its description); an Exists, Sink or Delete is
refused rather than running the program.

Native-only: the synchronous Steel engine reaches the async kernel through core's
`Invocation::scope_sync` bridge (real threads), so there is no wasm face yet. Builtin-set filtering by capability
(binding only the verbs a capability authorizes) is a later slice.

## Conformance

Passes [`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance):
`tests/conformance.rs` walks a fixture kernel binding `urn:lisp:eval`, a stored
program and the signed-run door beside the real `ikigai-sign` module the door
verifies through, with no opt-outs and every check running. Two decisions the
suite cannot hold are pinned there by hand:

- **An eval is live until its program says otherwise.** No door is pure and none
  is declared cacheable — a program can call any kernel verb, so an eval's
  cacheability is whatever its sub-resolutions carry, and one that touches
  nothing is still served uncacheable until the program itself opts in with
  `(cacheable …)`.
- **A gate inside a sub-resolution propagates typed.** A program that reaches a
  cap-gated resource under a capability that may run Lisp but holds no grant on
  that resource comes out as that resource's own `Denied`, on all three doors.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
