# Steel 0.8.3, embedded configuration

This directory contains the library source from the published `steel-core` 0.8.3
crate, upstream commit `11a57adcf42c22733463c59b1f44ec49eb7043bc`
(`crates/steel-core`). `UPSTREAM.json` records the registry archive's provenance.
The upstream MIT and Apache licenses are included. Upstream tests, benches and
Cargo metadata unrelated to building the library are omitted.

The only source changes are four configuration guards in `src/compiler/modules.rs`:
the new `embedded` feature makes `STEEL_HOME` return `None` and uses the existing
in-memory-only `CompiledModuleCache` constructor, previously used only on wasm.
This avoids home discovery, stderr diagnostics and directory creation. No Scheme
stdlib, macro, VM or sandbox behavior is changed. The feature is enabled directly
on ikigai-lisp's dependency, so downstream hosts also get it; a root-only Cargo
`[patch]` would not propagate to hosts.

Why vendor: Steel 0.8.3 has no public configuration for this behavior. Setting
`STEEL_HOME` in a library would mutate process-global environment and still allow
Steel to create `cached-modules/`. Upgrading Steel would invalidate the separately
audited sandbox allowlist. Keep the existing exact version and this patch until
an upstream embedding API can replace it. `tests/empty_home.rs` covers both an
empty home and existing/configured home directories in fresh child processes.

Do not publish this fork as a new Steel release. Before publishing ikigai-lisp,
resolve the path dependency to an upstream release providing this configuration
and re-audit the sandbox for that release.
