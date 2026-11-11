//! The eval ceiling, exercised end to end in its own process (an integration test
//! binary gets fresh globals, so the limits and the count of root evals in flight are
//! deterministic here — in-module unit tests share the process with
//! concurrently-running evals and can't pin the ceiling).

use std::sync::{Arc, OnceLock};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    Request, Verb,
};

static KERNEL: OnceLock<Arc<Kernel>> = OnceLock::new();

fn eval_of(program: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
}

/// With the ceiling pinned at 1, an UNRELATED eval — a fresh request at depth 0, the
/// host's, issued while another eval holds the only slot — is refused with the typed,
/// transient `Unavailable`, not a deadlock and not an unbounded second thread. An eval
/// NESTED inside the running one is not unrelated: it rides on its root's admission
/// and runs (ledger #3458), so one request's recursion meets the kernel's depth budget
/// rather than the host's pool.
#[test]
fn at_the_ceiling_an_unrelated_eval_is_a_typed_transient_refusal_and_a_nested_one_runs() {
    // This test binary is its own process, and this runs before any eval fixes the
    // limits.
    ikigai_lisp::set_limits(ikigai_lisp::Limits::default().workers(1))
        .expect("the first eval has not run");

    // `urn:probe` — invoked from INSIDE the outer eval (so the only slot is held) —
    // issues a second eval as a NEW root request, through the kernel at depth 0, and
    // reports what happened as its representation.
    let probe = FnEndpoint::new("probe", |_inv: &Invocation<'_>| {
        let kernel = KERNEL.get().expect("the kernel is built").clone();
        // FnEndpoint closures are synchronous; drive the issue with a no-parking poll
        // — the refusal is immediate (no slot, no await).
        let outcome =
            futures::FutureExt::now_or_never(kernel.issue(eval_of("(+ 1 2)"), &Capability::root()));
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
    let kernel = KERNEL
        .get_or_init(|| Arc::new(Kernel::new(Arc::new(space))))
        .clone();
    let cap = Capability::root();

    // The outer eval holds the single slot while its `(source "urn:probe")`
    // sub-request runs — exactly the window in which an unrelated eval must be
    // refused rather than spawn a second thread.
    let out = block_on(kernel.issue(eval_of("(source \"urn:probe\")"), &cap))
        .expect("outer eval completes");
    let text = String::from_utf8_lossy(&out.bytes);
    assert!(
        text.starts_with("refused-transient:"),
        "an unrelated eval at the ceiling must be a typed Unavailable, got: {text}"
    );
    assert!(text.contains("Limits::workers"), "refusal names the knob");

    // At the same ceiling, an eval nested inside the running one is admitted on the
    // outer eval's slot, and runs.
    let out = block_on(kernel.issue(
        eval_of("(invoke 'source \"urn:lisp:eval\" \"in\" \"(+ 1 2)\")"),
        &cap,
    ))
    .expect("a nested eval at the ceiling runs");
    assert_eq!(out.bytes, b"3");

    // The refusal is transient, not a wedge: with the outer eval finished (its
    // slot released), the same eval now runs on the idle worker.
    let out = block_on(kernel.issue(eval_of("(+ 1 2)"), &cap)).expect("a later eval succeeds");
    assert_eq!(out.bytes, b"3");
}
