//! Bounded inputs refuse with a typed error and never abort the host (audit round 6,
//! ledger #903: C-B4, C-B5, C-B6, C-B7).
//!
//! A stack overflow or a failed allocation is not a panic: it ABORTS the process. So
//! every scenario here runs in a CHILD process — this binary re-invoked on its `child`
//! test — and the parent asserts on the child's exit as well as its answer: an abort
//! shows up as a failed exit, not as this test binary dying. Inputs are the audit
//! reproductions' shapes (100,000 levels of nesting, a 10^14-element allocation).

use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

const CHILD: &str = "IKIGAI_LISP_BOUNDS_SCENARIO";
const DEEP: usize = 100_000;

fn nested_value(depth: usize) -> String {
    format!("(let loop ((i 0) (acc '())) (if (= i {depth}) acc (loop (+ i 1) (list acc))))")
}

fn eval(program: &str, data: Option<&str>) -> String {
    let kernel = Kernel::new(Arc::new(ikigai_lisp::space()));
    let mut request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
        .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()));
    if let Some(data) = data {
        request = request.with_arg("data", ArgRef::Inline(data.as_bytes().to_vec()));
    }
    match block_on(kernel.issue(request, &Capability::scoped(["urn:cap:lisp"]))) {
        Ok(out) => format!("Ok({})", String::from_utf8_lossy(&out.bytes)),
        Err(e) => format!("Err({e:?})"),
    }
}

/// One scenario, run in the child: its answer, as text.
fn scenario(name: &str) -> String {
    // Size bounds, at small limits this child sets for itself before anything fixes
    // the defaults.
    match name {
        "program-bytes" => {
            ikigai_lisp::set_limits(ikigai_lisp::Limits::default().max_program_bytes(64))
                .expect("the first thing this child does");
            return eval(&format!("(+ 1 {})", "1 ".repeat(64)), None);
        }
        "input-bytes" => {
            ikigai_lisp::set_limits(ikigai_lisp::Limits::default().max_input_bytes(64))
                .expect("the first thing this child does");
            return eval("(input)", Some(&"x".repeat(65)));
        }
        _ => {}
    }
    let max = ikigai_lisp::limits().max_nesting;
    match name {
        // C-B5: program text nested past the bound, and the audit's 100,000.
        "text-over" => eval(
            &format!("{}1{}", "(list ".repeat(max + 1), ")".repeat(max + 1)),
            None,
        ),
        "text-deep" => eval(
            &format!("{}1{}", "(list ".repeat(DEEP), ")".repeat(DEEP)),
            None,
        ),
        // At the bound it compiles: the worker's stack is sized for it.
        "text-at" => eval(
            &format!(
                "{}(length (list 1)){}",
                "(+ 0 ".repeat(max - 2),
                ")".repeat(max - 2)
            ),
            None,
        ),
        // The deepest compile found within the bounds: text near the bound around a
        // macro chain (`or` expands into nested `let`/`if`).
        "macro-at" => eval(
            &format!(
                "{}(or {}7){}",
                "(if #t ".repeat(max - 10),
                "#f ".repeat(250),
                " 2)".repeat(max - 10)
            ),
            None,
        ),
        // C-B6: data nested past the bound, through the prelude's `read`.
        "read-deep" => eval(
            "(read (open-input-string (input)))",
            Some(&format!("{}1{}", "(".repeat(DEEP), ")".repeat(DEEP))),
        ),
        "read-at" => eval(
            "(length (read (open-input-string (input))))",
            Some(&format!("{}1{}", "(".repeat(max), ")".repeat(max))),
        ),
        // C-B4: a value the program builds in a loop — no text bound sees it — crossing
        // into the s-expression adapter.
        "graph-deep" => eval(&format!("(graph {})", nested_value(DEEP)), None),
        "sparql-deep" => eval(&format!("(sparql-select {})", nested_value(DEEP)), None),
        // C-B7: one huge allocation. Neither function is on the sandbox's allowlist,
        // and no function that is takes a size: they allocate in proportion to what
        // they are given.
        "make-vector" => eval("(begin (make-vector 100000000000000 0) 1)", None),
        "make-string" => eval("(begin (make-string 100000000000000 #\\a) 1)", None),
        other => panic!("no scenario {other}"),
    }
}

/// The child: runs the scenario named in its environment and prints the answer.
#[test]
fn child() {
    if let Ok(name) = std::env::var(CHILD) {
        println!("ANSWER {}", scenario(&name));
    }
}

/// Run `name` in a child process; return its answer, asserting it exited cleanly.
fn in_child(name: &str) -> String {
    let out = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD, name)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{name}: the child did not exit cleanly ({:?}) — an abort is a host crash\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        .find_map(|line| line.split_once("ANSWER ").map(|(_, answer)| answer))
        .unwrap_or_else(|| panic!("{name}: no answer in\n{stdout}"))
        .trim_end_matches(" ok")
        .to_string()
}

fn assert_refused(name: &str, expected: &str) {
    let answer = in_child(name);
    assert!(
        answer.starts_with("Err(") && answer.contains(expected),
        "{name}: expected a typed refusal containing {expected:?}, got {answer}"
    );
}

#[test]
fn deep_program_text_is_refused_before_steel_parses_it() {
    for name in ["text-over", "text-deep"] {
        assert_refused(name, "InvalidArgument { name: \"in\"");
        assert_refused(name, "Limits::max_nesting");
    }
}

#[test]
fn programs_at_the_bound_compile_on_the_workers_stack() {
    assert_eq!(in_child("text-at"), "Ok(1)");
    assert_eq!(in_child("macro-at"), "Ok(7)");
}

#[test]
fn deep_data_is_refused_before_the_reader_parses_it() {
    assert_refused("read-deep", "Limits::max_nesting");
    assert_eq!(in_child("read-at"), "Ok(1)");
}

#[test]
fn a_deep_value_is_refused_at_the_sexpr_adapter() {
    assert_refused("graph-deep", "Limits::max_nesting");
    assert_refused("sparql-deep", "Limits::max_nesting");
}

#[test]
fn a_huge_allocation_is_refused_not_attempted() {
    assert_refused("make-vector", "Denied");
    assert_refused("make-string", "Denied");
}

#[test]
fn program_and_data_sizes_are_bounded() {
    assert_refused("program-bytes", "Limits::max_program_bytes");
    assert_refused("input-bytes", "InvalidArgument { name: \"data\"");
}
