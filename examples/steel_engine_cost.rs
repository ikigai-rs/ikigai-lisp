//! What building one bare Steel engine costs, with no ikigai code in the path: the
//! minimal reproduction of ledger #1100, where `steel-core` 0.8.3 builds an engine
//! about 1.6 times slower than 0.8.2 in release and about 2.5 times slower with Steel
//! unoptimized (a consumer's debug build). Every `urn:lisp:eval` builds one engine, so
//! this is the floor under the eval cost `eval_cost` measures.
//!
//! ```text
//! cargo run --release --example steel_engine_cost
//! cargo run --example steel_engine_cost --config 'profile.dev.package.steel-core.opt-level=0'
//! cargo run --release --example steel_engine_cost -- loop 5   # build for 5 s, for a profiler
//! ```
//!
//! To compare Steels, change the exact `steel-core` pin in `Cargo.toml` and move the
//! rest of the family with it (`steel-parser`, `steel-derive`, `steel-gen`,
//! `steel-quickscope`: `cargo update -p <name> --precise <version>`), so the comparison
//! is one Steel against another and not a mixed graph. Compare CPU time
//! (`/usr/bin/time -p`) as well as these wall-clock medians when the machine is busy.

use std::time::{Duration, Instant};

use steel::steel_vm::engine::Engine;

const N: usize = 15;

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("loop") {
        let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
        let until = Instant::now() + Duration::from_secs(secs);
        let mut builds = 0u32;
        while Instant::now() < until {
            drop(Engine::new_sandboxed());
            builds += 1;
        }
        println!("{builds} builds in {secs} s");
        return;
    }
    let build = median(
        (0..N)
            .map(|_| {
                let started = Instant::now();
                let engine = Engine::new_sandboxed();
                let elapsed = started.elapsed();
                drop(engine);
                elapsed
            })
            .collect(),
    );
    let run = median(
        (0..N)
            .map(|_| {
                let mut engine = Engine::new_sandboxed();
                let started = Instant::now();
                engine.run("(+ 1 2)").expect("(+ 1 2) evaluates");
                started.elapsed()
            })
            .collect(),
    );
    println!("Engine::new_sandboxed  median {build:>10.3?}   (n = {N})");
    println!("run (+ 1 2) on it      median {run:>10.3?}   (n = {N})");
}
