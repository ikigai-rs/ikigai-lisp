//! The minor findings of audit round 6 (ledger #903): C-B9, C-B10, C-R5, H-F3. Each
//! test takes the reproduction's input.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};

/// How many times a program reached the counter.
static WRITES: Mutex<usize> = Mutex::new(0);

fn kernel() -> Kernel {
    let counter = FnEndpoint::new("counter", |_inv: &Invocation<'_>| {
        *WRITES.lock().unwrap() += 1;
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"ok".to_vec(),
        ))
    });
    let fixtures = EndpointSpace::new()
        .bind(Exact::new("urn:test:counter"), counter)
        .bind(
            Exact::new("urn:test:count"),
            ikigai_lisp::program(
                "count",
                r#"(string-append "chars: " (number->string (string-length (input))))"#,
            ),
        )
        .bind(
            Exact::new("urn:test:sinks"),
            ikigai_lisp::program("sinks", r#"(sink "urn:test:counter" "x")"#),
        )
        .bind(
            Exact::new("urn:lisp:run"),
            ikigai_lisp::run_signed(["urn:test:nobody"]),
        );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(fixtures),
        Arc::new(ikigai_lisp::space()),
    ])))
}

fn lisp() -> Capability {
    Capability::scoped([ikigai_lisp::CAP_LISP, ikigai_lisp::CAP_LISP_RUN])
}

fn request(verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("valid IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    )
}

/// C-B9: the three doors declare Source (and Meta, which the kernel serves from the
/// description), but `invoke` never looked at the verb, so an Exists, a Sink or a
/// Delete RAN the program — an existence probe executing arbitrary code, sinks and all.
#[test]
fn c_b9_an_undeclared_verb_does_not_run_the_program() {
    let kernel = kernel();
    let program: &[u8] = br#"(sink "urn:test:counter" "x")"#;
    for verb in [Verb::Exists, Verb::Sink, Verb::Delete] {
        for target in [
            request(verb, "urn:lisp:eval", &[("in", program)]),
            request(verb, "urn:test:sinks", &[]),
            request(
                verb,
                "urn:lisp:run",
                &[("in", program), ("sig", b"x"), ("key", b"urn:test:nobody")],
            ),
        ] {
            let before = *WRITES.lock().unwrap();
            let outcome = block_on(kernel.issue(target.clone(), &lisp()));
            let writes = *WRITES.lock().unwrap() - before;
            assert!(
                matches!(outcome, Err(Error::InvalidArgument { ref name, .. }) if name == "verb")
                    && writes == 0,
                "{verb:?} on {}: {outcome:?}, sinks issued: {writes}",
                target.target
            );
        }
    }
}

/// C-B10: `(input)` data that is not UTF-8 was silently dropped — the program ran on
/// the empty string as if nothing had been sent. It is an error naming the argument.
#[test]
fn c_b10_data_that_is_not_utf8_is_refused_not_dropped() {
    let kernel = kernel();
    let latin1: &[u8] = b"caf\xe9 order 42";
    for (target, name) in [
        (
            request(
                Verb::Source,
                "urn:lisp:eval",
                &[
                    ("in", br#"(string-append "got [" (input) "]")"#),
                    ("data", latin1),
                ],
            ),
            "data",
        ),
        (
            request(Verb::Source, "urn:test:count", &[("content", latin1)]),
            "content",
        ),
    ] {
        let outcome = block_on(kernel.issue(target.clone(), &lisp()));
        assert!(
            matches!(&outcome, Err(Error::InvalidArgument { name: n, .. }) if n == name),
            "{}: {outcome:?}",
            target.target
        );
    }
}

/// H-F3: an eval with no program named `content` (the optional, piped alternative) as
/// missing, not `in`, the one the description requires.
#[test]
fn h_f3_a_missing_program_names_in() {
    let outcome = block_on(kernel().issue(request(Verb::Source, "urn:lisp:eval", &[]), &lisp()));
    assert!(
        matches!(&outcome, Err(Error::MissingArgument(name)) if name == "in"),
        "{outcome:?}"
    );
}

/// C-R5: an integral float outside i64 was rewritten to i64::MAX by `as i64`, which
/// saturates: `1e20` in a `(graph …)` became 9223372036854775807. Refused instead.
#[test]
fn c_r5_an_integral_float_outside_i64_is_refused_not_saturated() {
    let outcome = block_on(kernel().issue(
        request(
            Verb::Source,
            "urn:lisp:eval",
            &[(
                "in",
                br#"(graph '(graph (prefix (ex "http://example.org/")) (ex:a ex:b 1e20)))"#,
            )],
        ),
        &lisp(),
    ));
    match outcome {
        Err(e) => assert!(e.to_string().contains("i64"), "names the range: {e}"),
        Ok(out) => panic!(
            "1e20 was not refused: {}",
            String::from_utf8_lossy(&out.bytes)
        ),
    }
    // In range, an integral float still maps to an integer.
    let out = block_on(kernel().issue(
        request(
            Verb::Source,
            "urn:lisp:eval",
            &[(
                "in",
                br#"(graph '(graph (prefix (ex "http://example.org/")) (ex:a ex:b 1e3)))"#,
            )],
        ),
        &lisp(),
    ))
    .expect("1e3 is an integer");
    assert!(String::from_utf8_lossy(&out.bytes).contains("1000"));
}
