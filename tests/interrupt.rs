//! A runaway program releases its worker when its caller stops waiting (audit round 6,
//! ledger #903, C-B3). Before this, a `Timeout` overlay released the CALLER and left
//! the program spinning on its worker forever, so a ceiling's worth of runaways took
//! `urn:lisp:eval` and `urn:lisp:run` down until the process restarted.
//!
//! Own process, worker ceiling pinned at 2, one test at a time: the ceiling and the
//! pool are process globals, and a ceiling that runaways could fill is the point.
//!
//! **The callers stop waiting once every runaway is RUNNING, never at a clock started at
//! issue** (ledger #1685). A `Timeout` releases its caller by dropping the eval's future;
//! these tests drop it themselves, at the moment the last runaway reports it has started
//! (`urn:test:started`), which is the same drop at a time the machine's speed cannot
//! move. They used to race a 300 ms `Timeout` from issue against a fresh worker building
//! its engine, and under load the budget was spent on the builds: with 144 busy loops on
//! 18 cores no runaway ever started, the outcome was a canceled job, and all three tests
//! passed without a single program to interrupt. `tests/timeout_governor.rs` is where the
//! real overlay is shown firing mid-program.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use futures::executor::block_on;
use futures::future::{select, Either};
use ikigai_core::{
    ArgRef, Capability, Endpoint, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result as CoreResult, Verb,
};

const CEILING: usize = 2;

/// How long a caller retries an eval refused at the ceiling, and how long a released
/// worker may take to run an eval again. Only a wedge reaches it: a runaway that is not
/// interrupted spins forever, while a released worker is back after one engine build
/// (0.1 s idle, about 1.5 s with 144 busy loops on 18 cores, debug). So it is generous on
/// purpose, and a passing run never waits it out.
const WEDGED: Duration = Duration::from_secs(120);

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // This binary is its own process, and every test takes this lock before its
    // first eval; the first call fixes the limits, the rest find them fixed.
    let _ = ikigai_lisp::set_limits(ikigai_lisp::Limits::default().workers(CEILING));
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn eval(src: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()))
}

/// The marker a runaway resolves before it runs away. It is wrapped in a handler that
/// swallows its failure: when the callers stop waiting while a program is still parked
/// on the marker, the sub-request fails, and without the handler the program would end
/// on that failure instead of reaching its loop. With it, the only thing that can stop
/// the program afterward is the interrupt, so every ordering tests the interrupt.
const STARTED: &str = "(with-handler (lambda (e) #f) (source \"urn:test:started\"))";

/// One fill of the ceiling: how many runaways have started, and the callers to release
/// when the last one does.
#[derive(Default)]
struct Fill {
    started: usize,
    stops: Vec<oneshot::Sender<()>>,
}

/// Run a ceiling's worth of `program`s at once, each behind a marker, and stop waiting
/// on all of them once every one has started.
fn fill_the_ceiling_with(program: &'static str) {
    let fill = Arc::new(Mutex::new(Fill::default()));
    let marker = {
        let fill = Arc::clone(&fill);
        FnEndpoint::new("started", move |_inv: &Invocation<'_>| {
            let mut fill = fill.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            fill.started += 1;
            if fill.started == CEILING {
                for stop in fill.stops.drain(..) {
                    let _ = stop.send(());
                }
            }
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"started".to_vec(),
            ))
        })
    };
    let kernel = Arc::new(Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:started"), marker)),
        Arc::new(ikigai_lisp::space()),
    ]))));
    let source = format!("{STARTED} {program}");

    let callers = (0..CEILING)
        .map(|_| {
            let (stop_tx, stop_rx) = oneshot::channel::<()>();
            fill.lock().unwrap().stops.push(stop_tx);
            let kernel = Arc::clone(&kernel);
            let source = source.clone();
            std::thread::spawn(move || {
                let mut stop_rx = stop_rx;
                let since = Instant::now();
                let root = Capability::root();
                loop {
                    let issue = Box::pin(kernel.issue(eval(&source), &root));
                    match block_on(select(issue, stop_rx)) {
                        // Released while its runaway ran: the eval's future is dropped
                        // here, exactly as a fired `Timeout` drops it.
                        Either::Right(_) => return,
                        // A worker that an earlier runaway held is still on its way out
                        // (it builds one more engine first): wait for the slot.
                        Either::Left((Err(Error::Unavailable(_)), rx))
                            if since.elapsed() < WEDGED =>
                        {
                            stop_rx = rx;
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Either::Left((outcome, _)) => {
                            panic!("a runaway answered before every runaway started: {outcome:?}")
                        }
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    // A marker that never answers would leave the runaways spinning and this test
    // hanging; fail instead, and surface a caller that ended early with its own panic.
    let since = Instant::now();
    while fill.lock().unwrap().started < CEILING {
        if let Some(early) = callers.iter().position(|caller| caller.is_finished()) {
            // Released by the last start since the check above: not early after all.
            if fill.lock().unwrap().started == CEILING {
                break;
            }
            let caller = callers.into_iter().nth(early).expect("found above");
            caller.join().expect("caller thread");
            panic!("a caller stopped waiting before every runaway started");
        }
        assert!(
            since.elapsed() < WEDGED,
            "only {} of {CEILING} runaways started in {WEDGED:?}",
            fill.lock().unwrap().started
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for caller in callers {
        caller.join().expect("caller thread");
    }
}

/// Evaluate `(+ 20 22)` until it runs; at the ceiling every attempt is `Unavailable`
/// until a runaway's worker is released, and a runaway that is never interrupted holds
/// its worker forever.
fn an_eval_runs_again() {
    let plain = Kernel::new(Arc::new(ikigai_lisp::space()));
    let started = Instant::now();
    loop {
        match block_on(plain.issue(eval("(+ 20 22)"), &Capability::root())) {
            Ok(out) => {
                assert_eq!(out.bytes, b"42");
                return;
            }
            Err(Error::Unavailable(_)) if started.elapsed() < WEDGED => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(other) => panic!(
                "after {:?} eval is still wedged by the runaways: {other:?}",
                started.elapsed()
            ),
        }
    }
}

#[test]
fn a_ceiling_of_runaways_does_not_wedge_eval() {
    let _serial = serial();
    fill_the_ceiling_with("(let loop ((i 0)) (loop (+ i 1)))");
    an_eval_runs_again();
}

/// The interrupt cannot be caught: a handler is code, and the interrupt fires before
/// every instruction, so a runaway whose handler runs away too is still stopped.
#[test]
fn a_runaway_cannot_catch_the_interrupt() {
    let _serial = serial();
    fill_the_ceiling_with("(define (spin) (spin)) (with-handler (lambda (e) (spin)) (spin))");
    an_eval_runs_again();
}

// --- `urn:lisp:run` shares the pool, so a signed program must run again too ---

const K1_PRIV: &str = "-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIEIW/m80W4IrD82k3Mos0l4aeyfOkZMMZXqEYt6jpawc\n\
-----END PRIVATE KEY-----\n";
const K1_PUB: &str = "-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEAa9JuLzyLESJBF9LPZZ4RJk13iu5OhgKvLRQ3q0oQ4pE=\n\
-----END PUBLIC KEY-----\n";

struct StaticKey(&'static str);

#[async_trait::async_trait]
impl Endpoint for StaticKey {
    async fn invoke(&self, _inv: &Invocation<'_>) -> CoreResult<Representation> {
        Ok(Representation::new(
            ReprType::new("application/x-pem-file"),
            self.0.as_bytes().to_vec(),
        ))
    }
}

#[test]
fn a_ceiling_of_runaways_does_not_wedge_run() {
    let _serial = serial();
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:test:k1-priv"), StaticKey(K1_PRIV))
                .bind(Exact::new("urn:test:k1-pub"), StaticKey(K1_PUB))
                .bind(
                    Exact::new("urn:lisp:run"),
                    ikigai_lisp::run_signed(["urn:test:k1-pub"]),
                ),
        ),
        Arc::new(ikigai_sign::space()),
    ])));
    let program = "(+ 20 22)";
    let sig = block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:sign:sign").unwrap())
                .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
                .with_arg("key", ArgRef::Inline(b"urn:test:k1-priv".to_vec())),
            &Capability::root().attenuate(["urn:cap:sign".to_string()]),
        ),
    )
    .expect("signing succeeds")
    .bytes;

    fill_the_ceiling_with("(let loop ((i 0)) (loop (+ i 1)))");
    an_eval_runs_again();
    let out = block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:lisp:run").unwrap())
                .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
                .with_arg("sig", ArgRef::Inline(sig))
                .with_arg("key", ArgRef::Inline(b"urn:test:k1-pub".to_vec())),
            &Capability::root().attenuate([ikigai_lisp::CAP_LISP_RUN.to_string()]),
        ),
    )
    .expect("a signed program runs after the runaways");
    assert_eq!(out.bytes, b"42");
}
