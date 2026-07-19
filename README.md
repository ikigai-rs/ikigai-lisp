# ikigai-lisp

A [Steel](https://github.com/mattwparas/steel)-backed Lisp evaluator as an
[ikigai](https://github.com/ikigai-rs) module. One endpoint, `urn:lisp:eval`,
runs an s-expression whose builtins **are the kernel verbs** — `source`, `sink`,
`meta`, `exists`, `delete` — each issued back through the host kernel under the
eval's own capability. Code is a resource; the capability is the builtin set.

```text
source urn:lisp:eval '(source "urn:fn:toUpper" "hi")'   # -> HI
source urn:lisp:eval '(+ 1 2)'                           # -> 3
```

## Capability model

Two layers, both required:

1. **`urn:cap:lisp`** — gates "may run arbitrary Lisp at all," declared on the
   eval action's `requires` and enforced at entry.
2. **Per-verb enforcement** — every verb sub-request carries the eval's
   capability, so a `(sink …)` the capability doesn't authorize comes back as a
   typed `Denied`, surfaced to the program as a catchable Steel error
   (`with-handler`) — never a panic.

## Using it from a host

```rust,ignore
let space = ikigai_lisp::space(); // binds urn:lisp:eval
// mount into your kernel alongside the other modules
```

The eval result is **uncacheable** (an eval may mutate).

Slice 1 is native-only: the synchronous Steel engine is bridged to the async
kernel over an OS thread, so there is no wasm face yet. Builtin-set filtering by
capability (binding only the verbs a capability authorizes) and richer result
representations (Turtle for graph-shaped values) are later slices.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
