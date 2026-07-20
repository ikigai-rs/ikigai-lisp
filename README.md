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
   eval action's `requires` and enforced at entry.
2. **Per-verb enforcement** — every verb sub-request carries the eval's
   capability, so a `(sink …)` the capability doesn't authorize comes back as a
   typed `Denied`, surfaced to the program as a catchable Steel error
   (`with-handler`) — never a panic.

## Performance

Each eval runs on a fresh clone of a **warm, sandboxed Steel template** kept on a
pool of worker threads — the full standard library is loaded once and reused, not
rebuilt per call. Warm evals are on the order of **~0.2 ms** (vs. ~90 ms for a cold
`Engine::new()`), and every eval stays **isolated**: a `(define …)` in one eval
can't leak into the next.

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

## Using it from a host

```rust,ignore
let space = ikigai_lisp::space(); // binds urn:lisp:eval
// mount into your kernel alongside the other modules
```

Native-only: the synchronous Steel engine is bridged to the async kernel over the
worker pool, so there is no wasm face yet. Builtin-set filtering by capability
(binding only the verbs a capability authorizes) is a later slice.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
