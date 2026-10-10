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
//!
//! and the last two again for a program carrying a one-megabyte string literal (the
//! shape of a hostile input a host refuses by length), which costs about 7 ms of its
//! own on top of the engine build.
//!
//! A consumer's debug build compiles Steel unoptimized unless it says otherwise, and
//! that build costs about fifteen times more per engine. To see what such a host pays:
//!
//! ```text
//! cargo run --example eval_cost --config 'profile.dev.package.steel-core.opt-level=0'
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

const N: usize = 40;

fn eval(kernel: &Kernel, cap: &Capability, program: &[u8], want: &[u8]) {
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(program.to_vec()));
    let out = block_on(kernel.issue(request, cap)).expect("the program evaluates");
    assert_eq!(out.bytes, want);
}

fn timed(kernel: &Kernel, cap: &Capability, program: &[u8], want: &[u8]) -> Duration {
    let started = Instant::now();
    eval(kernel, cap, program, want);
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
    let small: &[u8] = b"(+ 1 2)";
    let big = format!("(string-length \"{}\")", "x".repeat(1_000_000)).into_bytes();
    // Long enough for a worker to finish building its next engine, even unoptimized.
    let pause = || std::thread::sleep(Duration::from_millis(1500));

    report("cold", vec![timed(&kernel, &cap, small, b"3")]);
    pause();
    report(
        "back-to-back",
        (0..N).map(|_| timed(&kernel, &cap, small, b"3")).collect(),
    );
    pause();
    report(
        "paced",
        (0..N)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(250));
                timed(&kernel, &cap, small, b"3")
            })
            .collect(),
    );
    pause();
    report(
        "1 MB b2b",
        (0..N)
            .map(|_| timed(&kernel, &cap, &big, b"1000000"))
            .collect(),
    );
    pause();
    report(
        "1 MB paced",
        (0..N)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(250));
                timed(&kernel, &cap, &big, b"1000000")
            })
            .collect(),
    );
}
