//! What one `urn:lisp:eval` costs, end to end through a kernel. The instrument
//! behind the isolation choice in the module docs: run it in release,
//!
//! ```text
//! cargo run --release --example eval_cost
//! ```
//!
//! It reports three numbers, each the median and the 95th percentile of `N`
//! evaluations of `(+ 1 2)`:
//!
//! - **cold**: the first eval on a new worker, which builds that worker's engine;
//! - **back-to-back**: evals issued with no pause, so a worker has no idle time
//!   between them;
//! - **paced**: evals issued with a pause between them, the shape of ordinary
//!   traffic (a reactor, a REPL, a request handler).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

const N: usize = 40;

fn eval(kernel: &Kernel, cap: &Capability) {
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(b"(+ 1 2)".to_vec()));
    let out = block_on(kernel.issue(request, cap)).expect("(+ 1 2) evaluates");
    assert_eq!(out.bytes, b"3");
}

fn timed(kernel: &Kernel, cap: &Capability) -> Duration {
    let started = Instant::now();
    eval(kernel, cap);
    started.elapsed()
}

fn report(label: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let median = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    println!(
        "{label:<13} median {median:>10.3?}   p95 {p95:>10.3?}   (n = {})",
        samples.len()
    );
}

fn main() {
    let kernel = Kernel::new(Arc::new(ikigai_lisp::space()));
    let cap = Capability::scoped([ikigai_lisp::CAP_LISP]);

    report("cold", vec![timed(&kernel, &cap)]);
    // Let the worker settle after its first job before measuring.
    std::thread::sleep(Duration::from_millis(500));
    report(
        "back-to-back",
        (0..N).map(|_| timed(&kernel, &cap)).collect(),
    );
    report(
        "paced",
        (0..N)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(250));
                timed(&kernel, &cap)
            })
            .collect(),
    );
}
