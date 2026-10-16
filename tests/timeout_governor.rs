//! The wire-eval L1 compute governor, end to end: a `Timeout` overlay over
//! `urn:lisp:eval` fires mid-program — which requires the eval's servicing loop
//! to YIELD (return `Pending`) rather than block its executor thread. Before the
//! async-bridge conversion the loop blocked in a crossbeam `recv`, the timer
//! never got polled, and a runaway program pinned the caller forever.
//!
//! Own process (integration binary): the worker ceiling and pool are globals.
//!
//! **The program is SEEN to start before the budget is believed** (ledger #1685). The
//! overlay's clock starts at issue, and a fresh worker builds its Steel engine before the
//! program runs: about 0.13 s idle in a debug build, about 1.4 s with 144 busy loops on
//! 18 cores, where a 300 ms budget expired during the build every time, the job was
//! canceled before it started, and this test passed without a program to cut off. So the
//! program reports its start (`urn:test:started`), and a budget that expired first is
//! doubled and tried again, until the overlay fires on a program that is running.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use ikigai_throttle::Timeout;

fn eval(src: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()))
}

/// How much later than its budget a caller may be released. Only a pinned caller
/// reaches it: a runaway that pins its caller pins it forever. Generous on purpose.
const PINNED: Duration = Duration::from_secs(30);

/// The budget at which a program that has never been seen to start stops being worth
/// waiting for: something other than the engine build is wrong.
const MOST_BUDGET: Duration = Duration::from_secs(120);

/// A pure busy-loop: no verb calls after its marker, so nothing inside it ever fails —
/// only the governor can end the CALLER's wait. The marker's own failure is swallowed:
/// when the budget expires while the program is parked on it, the program goes on to
/// its loop, so only the interrupt can stop it.
const RUNAWAY: &str = "(with-handler (lambda (e) #f) (source \"urn:test:started\")) \
                       (let loop ((i 0)) (loop (+ i 1)))";

#[test]
fn a_runaway_program_is_cut_off_at_the_wall_clock() {
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    let marker = FnEndpoint::new("started", |_inv: &Invocation<'_>| {
        STARTED.fetch_add(1, Ordering::SeqCst);
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"started".to_vec(),
        ))
    });
    // The served posture: the lisp space behind a Timeout overlay.
    let space = Arc::new(Fallback::new(vec![
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:started"), marker)),
        Arc::new(ikigai_lisp::space()),
    ]));
    let cap = Capability::root();

    let mut budget = Duration::from_millis(300);
    loop {
        let before = STARTED.load(Ordering::SeqCst);
        let kernel = Kernel::new(Arc::new(Timeout::new(Arc::clone(&space), budget)));
        // The caller waits on its own thread, so a caller the governor fails to release
        // fails this test instead of hanging it.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(block_on(kernel.issue(eval(RUNAWAY), &Capability::root())));
        });
        let outcome = rx
            .recv_timeout(budget + PINNED)
            .unwrap_or_else(|_| panic!("the caller is pinned past a {budget:?} budget"));
        let err = outcome.unwrap_err();
        assert!(
            matches!(err, Error::Timeout(_)),
            "the governor answers with a typed transient Timeout, got {err:?}"
        );
        assert!(
            err.is_transient(),
            "a timeout is transient (Retry/CB engage)"
        );
        if STARTED.load(Ordering::SeqCst) > before {
            break;
        }
        // The budget expired while the worker built its engine: nothing ran, so nothing
        // was cut off. Give the next fresh worker twice as long.
        budget *= 2;
        assert!(
            budget <= MOST_BUDGET,
            "the program never started under any budget up to {MOST_BUDGET:?}"
        );
    }

    // The interrupted worker's slot is accounted; a fresh eval still runs (a new
    // worker spawns under the ceiling). Recovery is asserted through an
    // un-overlaid kernel over the SAME global pool: a fresh worker builds its
    // Steel engine on first use, which can alone exceed the budget above —
    // pool recovery is the claim here, not engine build speed.
    let plain = Kernel::new(Arc::new(ikigai_lisp::space()));
    let out = block_on(plain.issue(eval("(+ 20 22)"), &cap)).expect("a later eval runs");
    assert_eq!(out.bytes, b"42");
}
