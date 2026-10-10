//! An uncaught verb failure leaves the eval AS ITSELF; a program's own error, even one
//! that quotes a failure it caught, stays the program's (ledger #980). Own process: the
//! lisp worker pool is a global.
//!
//! The failure list `retype_uncaught` consults holds every verb failure of the run,
//! CAUGHT ones included. It used to retype by substring, so a program that caught a
//! refusal and raised its own report quoting it came back as the quoted refusal, and the
//! report was thrown away (ikigai-programs' `drain.scm` had to avoid the kernel's own
//! `": "` spelling to survive it).

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    Representation, Request, Result as CoreResult, Verb,
};

/// A refusal whose text carries the characters a quoted rendering has to escape.
const AWKWARD: &str = "vault \"A\" is\nsealed\\";

fn kernel() -> Kernel {
    let refuse = |error: fn() -> Error| {
        move |_inv: &Invocation<'_>| -> CoreResult<Representation> { Err(error()) }
    };
    let fixtures = EndpointSpace::new()
        .bind(
            Exact::new("urn:test:denied"),
            FnEndpoint::new(
                "denied",
                refuse(|| Error::Denied("vault is sealed".to_string())),
            ),
        )
        .bind(
            Exact::new("urn:test:awkward"),
            FnEndpoint::new("awkward", refuse(|| Error::Denied(AWKWARD.to_string()))),
        )
        .bind(
            Exact::new("urn:test:busy"),
            FnEndpoint::new(
                "busy",
                refuse(|| Error::Unavailable("try later".to_string())),
            ),
        );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(fixtures),
        Arc::new(ikigai_lisp::space()),
    ])))
}

fn eval(src: &str) -> CoreResult<String> {
    block_on(
        kernel().issue(
            Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
                .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec())),
            &Capability::root(),
        ),
    )
    .map(|r| String::from_utf8(r.bytes).expect("UTF-8"))
}

#[test]
fn an_uncaught_refusal_leaves_the_eval_as_itself() {
    assert_eq!(
        eval(r#"(source "urn:test:denied")"#),
        Err(Error::Denied("vault is sealed".to_string()))
    );
}

#[test]
fn an_uncaught_refusal_with_quotes_and_newlines_stays_typed() {
    // The exact match compares against a quoted rendering; this pins that the quoting
    // Steel applies is the quoting `uncaught_text` reproduces.
    assert_eq!(
        eval(r#"(source "urn:test:awkward")"#),
        Err(Error::Denied(AWKWARD.to_string()))
    );
}

#[test]
fn a_programs_own_error_quoting_a_caught_refusal_stays_the_programs() {
    // The claim's reproduction: drain.scm's shape. Before the fix this came back as
    // `Denied("vault is sealed")`, with "drain: refused:" gone.
    let outcome = eval(
        r#"(with-handler
             (lambda (e) (error (string-append "drain: refused: " (to-string e))))
             (source "urn:test:denied"))"#,
    );
    match outcome {
        Err(Error::Endpoint(text)) => {
            assert!(text.contains("drain: refused: "), "{text}");
            assert!(text.contains("vault is sealed"), "{text}");
        }
        other => panic!("the program's own error was replaced: {other:?}"),
    }
}

#[test]
fn a_caught_refusal_re_raised_as_text_is_the_programs_error() {
    // `(error (to-string e))` adds Steel's own `Error: Generic: ` wrapper: what leaves is
    // the program's error, not the verb's.
    let outcome =
        eval(r#"(with-handler (lambda (e) (error (to-string e))) (source "urn:test:denied"))"#);
    assert!(
        matches!(outcome, Err(Error::Endpoint(_))),
        "the program's own error was replaced: {outcome:?}"
    );
}

#[test]
fn a_programs_error_spelled_like_the_refusal_is_still_the_programs() {
    // Word for word the kernel's text, but raised by `(error …)`, which renders it
    // differently from a verb's failure.
    let outcome = eval(
        r#"(with-handler (lambda (e) (error "denied: vault is sealed")) (source "urn:test:denied"))"#,
    );
    match outcome {
        Err(Error::Endpoint(text)) => assert!(text.contains("vault is sealed"), "{text}"),
        other => panic!("the program's own error was replaced: {other:?}"),
    }
}

#[test]
fn a_later_uncaught_failure_is_typed_after_a_caught_one() {
    // A caught refusal does not stop the NEXT verb failure, left uncaught, from leaving
    // the eval as itself — transient, so a Retry overlay can act on it.
    let outcome = eval(
        r#"(with-handler (lambda (e) "recovered") (source "urn:test:denied"))
           (source "urn:test:busy")"#,
    );
    assert_eq!(outcome, Err(Error::Unavailable("try later".to_string())));
}
