//! A runaway program releases its worker when its caller stops waiting (audit round 6,
//! ledger #903, C-B3). Before this, a `Timeout` overlay released the CALLER and left
//! the program spinning on its worker forever, so a ceiling's worth of runaways took
//! `urn:lisp:eval` and `urn:lisp:run` down until the process restarted.
//!
//! Own process, worker ceiling pinned at 2, one test at a time: the ceiling and the
//! pool are process globals, and a ceiling that runaways could fill is the point.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result as CoreResult, Verb,
};
use ikigai_throttle::Timeout;

const CEILING: usize = 2;

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // Safe: this binary is its own process, and every test takes this lock before
    // its first eval initializes the ceiling.
    std::env::set_var("IKIGAI_LISP_WORKERS", CEILING.to_string());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn eval(src: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()))
}

/// Run a ceiling's worth of `program`s at once behind a 300 ms `Timeout`, and check each
/// caller is released with a typed `Timeout`.
fn fill_the_ceiling_with(program: &'static str) {
    let callers: Vec<_> = (0..CEILING)
        .map(|_| {
            std::thread::spawn(move || {
                let governed = Kernel::new(Arc::new(Timeout::new(
                    ikigai_lisp::space(),
                    Duration::from_millis(300),
                )));
                block_on(governed.issue(eval(program), &Capability::root()))
            })
        })
        .collect();
    for caller in callers {
        let outcome = caller.join().expect("caller thread");
        assert!(
            matches!(outcome, Err(Error::Timeout(_))),
            "the governor answers a runaway with Timeout: {outcome:?}"
        );
    }
}

/// Evaluate `(+ 20 22)` until it runs, for up to 10 s; at the ceiling every attempt is
/// `Unavailable` until a runaway's worker is released.
fn an_eval_runs_again() -> Duration {
    let plain = Kernel::new(Arc::new(ikigai_lisp::space()));
    let started = Instant::now();
    loop {
        match block_on(plain.issue(eval("(+ 20 22)"), &Capability::root())) {
            Ok(out) => {
                assert_eq!(out.bytes, b"42");
                return started.elapsed();
            }
            Err(Error::Unavailable(_)) if started.elapsed() < Duration::from_secs(10) => {
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
    let waited = an_eval_runs_again();
    assert!(waited < Duration::from_secs(5), "released after {waited:?}");
}

/// The interrupt cannot be caught: a handler is code, and the interrupt fires before
/// every instruction, so a runaway whose handler runs away too is still stopped.
#[test]
fn a_runaway_cannot_catch_the_interrupt() {
    let _serial = serial();
    fill_the_ceiling_with("(define (spin) (spin)) (with-handler (lambda (e) (spin)) (spin))");
    let waited = an_eval_runs_again();
    assert!(waited < Duration::from_secs(5), "released after {waited:?}");
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
