//! `read` must not carry one call's leftovers into the next.
//!
//! Steel 0.8.2 keeps a single reader in a module-level global and, on a string
//! port whose text does not close, returns eof without clearing it. The partial
//! form then sits there for the life of the worker: every later `read` appends
//! its port to the stale fragment and reports `(eof)`. In a serial reactor that
//! is one malformed tuple silently converting every request queued behind it
//! into a parse failure — damage arriving long after its cause.
//!
//! Its own test binary, and the worker ceiling pinned at 1, so every eval here
//! runs on the SAME worker: that is the only arrangement in which a leak
//! between evals is observable rather than luck.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

/// Evaluate `src`, rendering an error as `ERR: …` so a test can assert on either.
fn eval(kernel: &Kernel, src: &str) -> String {
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(repr) => String::from_utf8_lossy(&repr.bytes).to_string(),
        Err(error) => format!("ERR: {error}"),
    }
}

/// One test, not four: the ceiling is process-global, so pinning it at 1 only
/// pins anything while a single test owns the process.
#[test]
fn read_carries_nothing_between_calls() {
    // Safe: this test binary is its own process, and this runs before any eval
    // initializes the ceiling.
    std::env::set_var("IKIGAI_LISP_WORKERS", "1");
    let kernel = Kernel::new(Arc::new(ikigai_lisp::space()));

    assert_eq!(
        eval(&kernel, r#"(read (open-input-string "(a b c)"))"#),
        "(a b c)",
        "a well-formed datum reads as itself"
    );

    // The poisoning input. It must be a NAMED failure the caller can act on —
    // before this fix it was `(eof)`, which is indistinguishable from an empty
    // input and is what let the damage travel silently.
    let unterminated = eval(&kernel, r#"(read (open-input-string "(a b c"))"#);
    assert!(
        unterminated.starts_with("ERR:") && unterminated.contains("ends inside a form"),
        "an unterminated form must raise, naming the cause; got: {unterminated}"
    );

    // …and it must be catchable, so a handler can answer the request rather than
    // the whole eval dying.
    assert_eq!(
        eval(
            &kernel,
            r#"(with-handler (lambda (e) "caught") (read (open-input-string "(a b c")))"#
        ),
        "caught",
        "the refusal is a catchable Steel error, not a panic"
    );

    // THE REGRESSION: every later read on this worker, in a LATER eval, is
    // unaffected. Two of them, because the failure mode was permanent.
    assert_eq!(
        eval(&kernel, r#"(read (open-input-string "(a b c)"))"#),
        "(a b c)",
        "a read after an unterminated one is unaffected"
    );
    assert_eq!(
        eval(&kernel, r#"(read (open-input-string "(x y z)"))"#),
        "(x y z)",
        "and stays unaffected"
    );

    // The leak also has to be sealed WITHIN one eval — a reactor drains several
    // tuples per program, so the bad one sits between two good ones in one run.
    let out = eval(
        &kernel,
        r#"(define (try text)
             (with-handler (lambda (e) "refused") (read (open-input-string text))))
           (list (try "(good 1)") (try "(bad") (try "(good 2)"))"#,
    );
    assert_eq!(
        out, "((good 1) \"refused\" (good 2))",
        "a malformed port between two sound ones affects only itself"
    );

    // Blank input is eof, not a diagnosis about an unclosed form — including the
    // comment-only case, where the reader also ends with unconsumed residue.
    for text in ["", "   ", "\\n\\t ", "; just a comment"] {
        let src = format!(r#"(eof-object? (read (open-input-string "{text}")))"#);
        assert_eq!(eval(&kernel, &src), "#true", "{text:?} reads as eof");
    }

    // Successive reads of ONE port still walk its datums in order, then report
    // eof — the per-port reader is what makes that work without a global.
    assert_eq!(
        eval(
            &kernel,
            r#"(define p (open-input-string "(a) (b) 3"))
               (list (read p) (read p) (read p) (eof-object? (read p)))"#,
        ),
        "((a) (b) 3 #true)"
    );
}
