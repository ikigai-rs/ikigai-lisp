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

## Performance

Each eval runs on a fresh clone of a **warm, sandboxed Steel template** kept on a
pool of worker threads — the full standard library is loaded once and reused, not
rebuilt per call. Warm evals are on the order of **~0.2 ms** (vs. ~90 ms for a cold
`Engine::new()`), and every eval stays **isolated**: a `(define …)` in one eval
can't leak into the next.

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

### `read` carries nothing between calls

Steel 0.8.2's `read` keeps one reader in a shared object and, on a string or file
port whose text does not close, returns eof while **leaving the partial form in
it** — so every later `read` on that worker appends its port to the stale fragment
and reports `(eof)` for the life of the process. This crate shadows `read` for
string and file ports with one whose reader is keyed to the port it drained: a
port not already drained gets a fresh reader, successive reads of one port still
walk its datums in order, and input that ends inside a form raises a catchable
error naming the cause instead of quietly reading as eof.

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
// mount into your kernel alongside the other modules
```

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
