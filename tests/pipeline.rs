//! Pipeline citizenship for the three doors, through the **engine** — the only place it
//! is visible (ledger #980).
//!
//! Whether `source urn:booking:confirm (pending)` or `… | urn:booking:confirm` reaches a
//! door is decided by a rule in `ikigai-engine`, not in the kernel: a Source fills **the
//! one declared argument left unnamed**, and when several are unnamed it keeps only the
//! REQUIRED ones, so two optional inputs leave nothing to route to and the line is
//! refused as ambiguous. A kernel-level test names its argument and cannot see any of
//! this, and neither can the conformance suite. So this file drives the real engine
//! over the real doors.
//!
//! ⚠ The engine fetches each door's contract as `Meta as=application/json`, and when that
//! fails it fails OPEN, routing every value to an input named `in`. On a kernel with no
//! JSON meta renderer every line below "works", whatever the doors declare, so the kernel
//! here is built with `Kernel::with_meta_renderer`, and the last test is the witness
//! that the contract is really being read.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Kernel, MetaRenderer,
    ReprType, Representation,
};
use ikigai_engine::{Action, Engine};

/// The contract renderer the engine actually asks for. Without it these tests would pass
/// over doors whose arguments could be named anything.
struct JsonRenderer;

impl MetaRenderer for JsonRenderer {
    fn render(
        &self,
        description: &Description,
        _target: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

/// The text an upstream stage hands down the pipe: a Lisp program, which is code to
/// `urn:lisp:eval` and plain data to a program door.
const PIPED: &str = "(+ 40 2)";

fn space() -> Fallback {
    let fixtures = EndpointSpace::new()
        .bind(
            Exact::new("urn:test:piped"),
            FnEndpoint::new("piped", |_inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    PIPED.as_bytes().to_vec(),
                ))
            }),
        )
        .bind(
            Exact::new("urn:test:echo"),
            ikigai_lisp::program("echo", r#"(string-append "got: " (input))"#),
        )
        .bind(
            Exact::new("urn:test:bare"),
            ikigai_lisp::program("bare", "(+ 1 2)"),
        )
        .bind(
            // A real key IRI is not needed: a routed value reaches the door, which refuses
            // the untrusted key — a refusal only the DOOR can make. A value the engine
            // could not route never gets that far.
            Exact::new("urn:lisp:run"),
            ikigai_lisp::run_signed(["urn:test:trusted"]),
        );
    Fallback::new(vec![Arc::new(fixtures), Arc::new(ikigai_lisp::space())])
}

fn engine() -> Engine {
    Engine::new(Kernel::with_meta_renderer(
        Arc::new(space()),
        Arc::new(JsonRenderer),
    ))
}

fn run_on(engine: &Engine, line: &str) -> Result<String, String> {
    match block_on(engine.eval_async(line)) {
        Action::Output(entry) => entry.result,
        _ => Err(format!("`{line}` produced no output")),
    }
}

fn run(line: &str) -> Result<String, String> {
    run_on(&engine(), line)
}

// ---- a stored program door: `in`, optional, the only declared input ----------------

#[test]
fn a_positional_value_reaches_a_program_door_as_its_input() {
    let out = run("source urn:test:echo hello").expect("the positional value routes");
    assert_eq!(out.trim_end(), "got: hello");
}

#[test]
fn a_piped_value_reaches_a_program_door_as_its_input() {
    // The value is a program's text, and the door treats it as DATA: it is echoed, not
    // evaluated (`42` would mean it ran).
    let out = run("source urn:test:piped | urn:test:echo").expect("the piped value routes");
    assert_eq!(out.trim_end(), format!("got: {PIPED}"));
}

#[test]
fn a_named_in_still_reaches_a_program_door() {
    let out = run(r#"source urn:test:echo in="(pending)""#).expect("in= routes by name");
    assert_eq!(out.trim_end(), "got: (pending)");
}

#[test]
fn a_program_door_still_runs_with_no_input_at_all() {
    // `in` stays OPTIONAL: a program that reads no input (a drain run by a schedule) is
    // called bare, and the manifold must not claim it needs anything.
    let out = run("source urn:test:bare").expect("a bare call runs the program");
    assert_eq!(out.trim_end(), "3");
}

// ---- urn:lisp:eval: `in` required, so it routes even beside optional inputs --------

#[test]
fn a_positional_program_reaches_eval() {
    let out = run("source urn:lisp:eval 42").expect("the positional program routes");
    assert_eq!(out.trim_end(), "42");
}

#[test]
fn a_piped_program_reaches_eval() {
    let out = run("source urn:test:piped | urn:lisp:eval").expect("the piped program routes");
    assert_eq!(out.trim_end(), "42");
}

// ---- urn:lisp:run: `in`/`sig`/`key` named, the value is the unsigned `data` ---------

/// A value that reached the signed-run door is refused BY THE DOOR for its untrusted key;
/// one the engine could not route never reaches it.
fn assert_reached_the_run_door(outcome: Result<String, String>) {
    let err = outcome.expect_err("an untrusted key is refused");
    assert!(
        err.contains("code-signing trust set"),
        "the value never reached urn:lisp:run: {err}"
    );
}

#[test]
fn a_positional_value_reaches_the_signed_run_door_as_its_data() {
    assert_reached_the_run_door(run(
        r#"source urn:lisp:run in="(input)" sig=x key=urn:test:untrusted hello"#,
    ));
}

#[test]
fn a_piped_value_reaches_the_signed_run_door_as_its_data() {
    assert_reached_the_run_door(run(
        r#"source urn:test:piped | urn:lisp:run in="(input)" sig=x key=urn:test:untrusted"#,
    ));
}

// ---- the witness ----------------------------------------------------------------------

#[test]
fn the_tests_above_would_notice_a_routing_defect() {
    // The witness for the ⚠ at the top of this file. On a kernel that cannot answer
    // `Meta as=application/json`, the engine routes every value to `in`, so it cannot
    // refuse an ambiguous contract. On this kernel it can: name every input of eval but
    // two optional ones, and the engine must refuse the positional value as ambiguous
    // rather than guess — which it can only do by READING the contract.
    let err = run("source urn:lisp:eval in=42 dialect=steel hello")
        .expect_err("two unnamed optional inputs are ambiguous");
    assert!(
        err.contains("accepts multiple arguments"),
        "the engine did not read the contract: {err}"
    );
}
