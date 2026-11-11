//! A program that calls itself through the kernel (ledger #3458).
//!
//! Each level of the recursion is an eval nested inside the eval that issued it, and
//! the kernel bounds that nesting (`Kernel::with_max_depth`, default 64). Before this
//! test, every level checked out a worker of its own from the host's pool, so with a
//! pool smaller than the depth budget the recursion ran out of workers first and was
//! refused as `Unavailable` ("all N eval workers are busy (transient — retry…)"): a
//! retry of a program that can never finish, while it held every worker on the host.
//!
//! Its own process (an integration test binary), so the limits are this file's alone.

use std::sync::{Arc, OnceLock};

use futures::executor::block_on;
use ikigai_core::{
    Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};

/// Fewer workers than the kernel's depth budget, so the recursion meets the pool's
/// bound first unless nested evals stay off it.
const WORKERS: usize = 4;

static KERNEL: OnceLock<Arc<Kernel>> = OnceLock::new();

fn kernel() -> Arc<Kernel> {
    KERNEL
        .get_or_init(|| {
            ikigai_lisp::set_limits(ikigai_lisp::Limits::default().workers(WORKERS))
                .expect("the first eval has not run");
            // `urn:probe` issues an UNRELATED request, a fresh root eval at depth 0 from
            // its own thread, while the recursion that reached it holds its workers, and
            // answers with what happened to it.
            let probe = FnEndpoint::new("probe", |_inv: &Invocation<'_>| {
                let text = std::thread::spawn(|| {
                    let request = Request::new(Verb::Source, iri("urn:lisp:eval"))
                        .with_arg("in", ikigai_core::ArgRef::Inline(b"(+ 1 2)".to_vec()));
                    match block_on(kernel().issue(request, &Capability::root())) {
                        Ok(out) => String::from_utf8_lossy(&out.bytes).into_owned(),
                        Err(e) => format!("refused: {e:?}"),
                    }
                })
                .join()
                .expect("the probe thread completes");
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    text.into_bytes(),
                ))
            });
            let space = Fallback::new(vec![
                Arc::new(
                    EndpointSpace::new()
                        .bind(
                            Exact::new("urn:self"),
                            ikigai_lisp::program("urn:self", "(source \"urn:self\")"),
                        )
                        // Recurses through the kernel until (input) levels deep, then
                        // reports what an unrelated eval meets meanwhile.
                        .bind(
                            Exact::new("urn:countdown"),
                            ikigai_lisp::program(
                                "urn:countdown",
                                "(let ((n (string->number (input)))) \
                                   (if (= n 0) \
                                       (source \"urn:probe\") \
                                       (invoke 'source \"urn:countdown\" \"in\" \
                                               (number->string (- n 1)))))",
                            ),
                        )
                        .bind(Exact::new("urn:probe"), probe),
                ),
                Arc::new(ikigai_lisp::space()),
            ]);
            Arc::new(Kernel::new(Arc::new(space)))
        })
        .clone()
}

fn iri(text: &str) -> Iri {
    Iri::parse(text).expect("valid IRI")
}

/// The self-call is refused as the depth refusal it is, permanent and typed, never as
/// a busy pool a caller is told to retry.
#[test]
fn a_self_calling_program_is_refused_as_too_deep_not_as_a_busy_pool() {
    let err = block_on(kernel().issue(
        Request::new(Verb::Source, iri("urn:self")),
        &Capability::root(),
    ))
    .expect_err("a program that sources itself never finishes");
    assert!(
        matches!(err, Error::DepthExceeded { .. }),
        "a self-call must meet the kernel's depth bound, got: {err:?}"
    );
    assert!(!err.is_transient(), "too deep is permanent: {err:?}");

    // Nothing is left held: an ordinary eval runs afterwards.
    let out = block_on(
        kernel().issue(
            Request::new(Verb::Source, iri("urn:lisp:eval"))
                .with_arg("in", ikigai_core::ArgRef::Inline(b"(+ 1 2)".to_vec())),
            &Capability::root(),
        ),
    )
    .expect("an eval after the refusal runs");
    assert_eq!(out.bytes, b"3");
}

/// A recursion deeper than the pool does not hold the pool: while it is parked at its
/// deepest level, an unrelated eval is still admitted.
#[test]
fn a_recursion_deeper_than_the_pool_leaves_room_for_unrelated_evals() {
    let depth = (WORKERS * 3).to_string();
    let out = block_on(
        kernel().issue(
            Request::new(Verb::Source, iri("urn:countdown"))
                .with_arg("in", ikigai_core::ArgRef::Inline(depth.into_bytes())),
            &Capability::root(),
        ),
    )
    .expect("a recursion within the depth budget completes");
    assert_eq!(
        String::from_utf8_lossy(&out.bytes),
        "3",
        "the unrelated eval issued at the recursion's deepest point must run"
    );
}
