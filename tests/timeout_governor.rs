//! The wire-eval L1 compute governor, end to end: a `Timeout` overlay over
//! `urn:lisp:eval` fires mid-program — which requires the eval's servicing loop
//! to YIELD (return `Pending`) rather than block its executor thread. Before the
//! async-bridge conversion the loop blocked in a crossbeam `recv`, the timer
//! never got polled, and a runaway program pinned the caller forever.
//!
//! Own process (integration binary): the worker ceiling and pool are globals.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_throttle::Timeout;

fn eval(src: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()))
}

#[test]
fn a_runaway_program_is_cut_off_at_the_wall_clock() {
    // The served posture: the lisp space behind a Timeout overlay.
    let kernel = Kernel::new(Arc::new(Timeout::new(
        ikigai_lisp::space(),
        Duration::from_millis(300),
    )));
    let cap = Capability::root();

    // A pure busy-loop: no verb calls, so nothing inside it ever fails — only
    // the governor can end the CALLER's wait. (The worker thread itself keeps
    // spinning — native Steel cannot be preempted; the worker-ceiling bounds how
    // many such runaways can ever exist. Deterministic preemption is L2/wasm.)
    let started = Instant::now();
    let err = block_on(kernel.issue(eval("(let loop ((i 0)) (loop (+ i 1)))"), &cap)).unwrap_err();
    assert!(
        matches!(err, Error::Timeout(_)),
        "the governor answers with a typed transient Timeout, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the caller is released at the budget, not pinned: {:?}",
        started.elapsed()
    );
    assert!(
        err.is_transient(),
        "a timeout is transient (Retry/CB engage)"
    );

    // The burned worker's slot is accounted; a fresh eval still runs (a new
    // worker spawns under the ceiling). Recovery is asserted through an
    // un-overlaid kernel over the SAME global pool: a fresh worker builds its
    // Steel template on first use, which in a debug build can alone exceed the
    // tight test budget above — pool recovery is the claim here, not template
    // build speed.
    let plain = Kernel::new(Arc::new(ikigai_lisp::space()));
    let out = block_on(plain.issue(eval("(+ 20 22)"), &cap)).expect("a later eval runs");
    assert_eq!(out.bytes, b"42");
}
