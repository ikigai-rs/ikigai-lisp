//! The eval-worker ceiling, exercised end to end in its own process (an
//! integration test binary gets fresh globals, so the env override and the
//! LIVE_WORKERS count are deterministic here — in-module unit tests share the
//! process with concurrently-running evals and can't pin the ceiling).

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    Request, Verb,
};

/// With the ceiling pinned at 1, a NESTED eval — a program whose sub-request
/// resolves another `urn:lisp:eval` while the outer program still holds the only
/// worker — is refused with the typed, transient `Unavailable`, not a deadlock
/// and not an unbounded second thread.
#[test]
fn at_the_ceiling_a_nested_eval_is_a_typed_transient_refusal() {
    // Safe: this test binary is its own process and this runs before any eval
    // initializes the ceiling.
    std::env::set_var("IKIGAI_LISP_WORKERS", "1");

    // `urn:probe` — invoked from INSIDE the outer eval (so the outer worker is
    // checked out and busy) — attempts a second, nested eval and reports what
    // happened as its representation.
    let probe = FnEndpoint::new("probe", |inv: &Invocation<'_>| {
        let nested = Request::new(
            Verb::Source,
            Iri::parse("urn:lisp:eval").expect("valid IRI"),
        )
        .with_arg("in", ArgRef::Inline(b"(+ 1 2)".to_vec()));
        // FnEndpoint closures are synchronous; drive the nested issue with a
        // no-parking poll — the refusal is immediate (no worker, no await).
        let outcome = futures::FutureExt::now_or_never(inv.issue(nested));
        let text = match outcome {
            Some(Err(Error::Unavailable(msg))) => format!("refused-transient: {msg}"),
            Some(Err(other)) => format!("wrong-error: {other:?}"),
            Some(Ok(_)) => "unexpectedly-ran".to_string(),
            None => "parked".to_string(),
        };
        Ok(ikigai_core::Representation::new(
            ikigai_core::ReprType::new("text/plain"),
            text.into_bytes(),
        ))
    });

    let space = Fallback::new(vec![
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:probe"), probe)),
        Arc::new(ikigai_lisp::space()),
    ]);
    let kernel = Kernel::new(Arc::new(space));
    let cap = Capability::root();

    // The outer eval holds the single worker while its `(source "urn:probe")`
    // sub-request runs — exactly the window in which the nested checkout must
    // refuse rather than spawn a second thread.
    let outer = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(b"(source \"urn:probe\")".to_vec()));
    let out = block_on(kernel.issue(outer, &cap)).expect("outer eval completes");
    let text = String::from_utf8_lossy(&out.bytes);
    assert!(
        text.starts_with("refused-transient:"),
        "the nested eval at the ceiling must be a typed Unavailable, got: {text}"
    );
    assert!(
        text.contains("IKIGAI_LISP_WORKERS"),
        "refusal names the knob"
    );

    // The refusal is transient, not a wedge: with the outer eval finished (its
    // worker back in the pool), the same eval now runs on the idle worker
    // without needing a new slot.
    let again = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(b"(+ 1 2)".to_vec()));
    let out = block_on(kernel.issue(again, &cap)).expect("a later eval succeeds");
    assert_eq!(out.bytes, b"3");
}
