//! Nothing one evaluation defines is visible to another: not a global, not a
//! macro, not a symbol's binding. Audit round 6 (ledger #903, C-B1, C-B2, C-B8)
//! found that the per-eval clone isolated VALUES but shared the compiler, so a
//! `define-syntax` from one eval rewrote the next caller's program, and a name
//! from an earlier eval read as `#<void>` instead of unbound.
//!
//! Its own test binary with the worker ceiling pinned at 1, so consecutive evals
//! land on the SAME worker: the only arrangement in which a leak between evals is
//! observable rather than luck. The tests take a lock so they run one at a time
//! (at ceiling 1 a concurrent second eval would be refused `Unavailable`).

use std::sync::{Arc, Mutex, MutexGuard};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Endpoint, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result as CoreResult, Verb,
};

/// One eval at a time, every eval on the one worker.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // This binary is its own process, and every test takes this lock before its
    // first eval; the first call fixes the limits, the rest find them fixed.
    let _ = ikigai_lisp::set_limits(ikigai_lisp::Limits::default().workers(1));
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// The same fixed Ed25519 fixtures `tests/signed_run.rs` uses: k1 signs code.
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

/// What every eval in this binary sank to the drop box, in order.
static DROPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn dropped() -> Vec<String> {
    DROPPED.lock().unwrap().clone()
}

/// A kernel with a secret only `urn:cap:test:secret` may read, a drop box anyone
/// may sink to (standing in for any exfiltration channel), the lisp space, the
/// real sign module, and a `urn:lisp:run` that trusts k1.
fn kernel() -> Kernel {
    let secret = FnEndpoint::new("secret", |inv: &Invocation<'_>| {
        if !inv.capability.allows("urn:cap:test:secret") {
            return Err(Error::Denied("no secret grant".into()));
        }
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"TOP-SECRET".to_vec(),
        ))
    });
    let dropbox = FnEndpoint::new("dropbox", |inv: &Invocation<'_>| {
        let body = inv.inline_str("content").unwrap_or("").to_string();
        DROPPED.lock().unwrap().push(body);
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"ok".to_vec(),
        ))
    });
    let fixtures = EndpointSpace::new()
        .bind(Exact::new("urn:test:secret"), secret)
        .bind(Exact::new("urn:test:dropbox"), dropbox)
        .bind(Exact::new("urn:test:k1-priv"), StaticKey(K1_PRIV))
        .bind(Exact::new("urn:test:k1-pub"), StaticKey(K1_PUB))
        .bind(
            Exact::new("urn:lisp:run"),
            ikigai_lisp::run_signed(["urn:test:k1-pub"]),
        );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(fixtures),
        Arc::new(ikigai_lisp::space()),
        Arc::new(ikigai_sign::space()),
    ])))
}

fn eval(kernel: &Kernel, cap: &Capability, src: &str) -> Result<String, Error> {
    let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
        .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()));
    block_on(kernel.issue(request, cap)).map(|r| String::from_utf8(r.bytes).unwrap())
}

/// Only `urn:cap:lisp`: the least a caller can hold and still run code.
fn lisp_only() -> Capability {
    Capability::scoped([ikigai_lisp::CAP_LISP])
}

/// The macro hijack (C-B1): a caller holding only `urn:cap:lisp` redefines
/// `source` as a macro that copies what it reads to the drop box. The NEXT
/// caller, who may read the secret, must run its own program, not the macro.
#[test]
fn a_macro_from_one_eval_does_not_rewrite_the_next() {
    let _serial = serial();
    let kernel = kernel();
    let before = dropped().len();
    let planted = eval(
        &kernel,
        &lisp_only(),
        r#"(define-syntax source
             (syntax-rules ()
               ((_ iri) (let ((v (%source iri))) (%sink "urn:test:dropbox" v) v))))
           "planted""#,
    );
    assert_eq!(planted.as_deref().ok(), Some("planted"), "{planted:?}");

    let victim = Capability::scoped([ikigai_lisp::CAP_LISP, "urn:cap:test:secret"]);
    let read = eval(&kernel, &victim, r#"(source "urn:test:secret")"#);
    assert_eq!(read.as_deref().ok(), Some("TOP-SECRET"), "{read:?}");
    assert_eq!(
        dropped()[before..],
        [] as [String; 0],
        "the next caller's (source …) ran the planted macro under the next caller's capability"
    );
}

/// The signed-program rewrite (C-B2): the bytes `urn:sign:verify` checked must be
/// what runs. An unsigned eval's macro must not reach a verified program.
#[test]
fn an_unsigned_macro_does_not_rewrite_a_verified_signed_program() {
    let _serial = serial();
    let kernel = kernel();
    let before = dropped().len();
    eval(
        &kernel,
        &lisp_only(),
        r#"(define-syntax source
             (syntax-rules ()
               ((_ iri) (let ((v (%source iri))) (%sink "urn:test:dropbox" v) "nothing here"))))
           "planted""#,
    )
    .expect("the planting eval runs");

    let program = r#"(source "urn:test:secret")"#;
    let signer = Capability::root().attenuate(["urn:cap:sign".to_string()]);
    let sig = block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:sign:sign").unwrap())
                .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
                .with_arg("key", ArgRef::Inline(b"urn:test:k1-priv".to_vec())),
            &signer,
        ),
    )
    .expect("signing succeeds")
    .bytes;
    let submitter = Capability::root().attenuate([
        ikigai_lisp::CAP_LISP_RUN.to_string(),
        "urn:cap:test:secret".to_string(),
    ]);
    let out = block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:lisp:run").unwrap())
                .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
                .with_arg("sig", ArgRef::Inline(sig))
                .with_arg("key", ArgRef::Inline(b"urn:test:k1-pub".to_vec())),
            &submitter,
        ),
    )
    .expect("the signed program verifies and runs");
    assert_eq!(String::from_utf8(out.bytes).unwrap(), "TOP-SECRET");
    assert_eq!(
        dropped()[before..],
        [] as [String; 0],
        "the verified bytes are not what ran"
    );
}

/// The macro data leak: a macro's literal is a value one eval can plant and
/// another can read. Every way of defining a macro is covered, so the guarantee
/// is not a list of the forms someone thought of. (`defmacro` no longer gets as far
/// as planting anything: the sandbox refuses it, because its body runs at expansion
/// time in the compiler's macro engine. It stays here so the guarantee still covers it.)
#[test]
fn a_macro_defined_in_one_eval_is_not_visible_in_the_next() {
    let _serial = serial();
    let kernel = kernel();
    for (plant, probe, refused) in [
        (
            r#"(define-syntax leaked-rules (syntax-rules () ((_) "MACRO-SECRET"))) 0"#,
            "(leaked-rules)",
            false,
        ),
        (
            r#"(defmacro (leaked-defmacro) "MACRO-SECRET") 0"#,
            "(leaked-defmacro)",
            true,
        ),
    ] {
        let planted = eval(&kernel, &lisp_only(), plant);
        if refused {
            assert!(
                matches!(planted, Err(Error::Denied(_))),
                "`{plant}` must be refused: {planted:?}"
            );
        } else {
            planted.unwrap_or_else(|e| panic!("{plant}: {e}"));
        }
        let seen = eval(&kernel, &lisp_only(), probe);
        assert!(
            seen.is_err(),
            "the next eval saw the macro planted by `{plant}`: {probe} => {seen:?}"
        );
    }
}

/// A name from an earlier eval is UNBOUND in a later one, whatever the later
/// program defines first (C-B8). With a shared compiler the old name kept its
/// global slot, and once the later program grew the table past it, it read as a
/// truthy `#<void>` instead of failing.
#[test]
fn a_name_from_an_earlier_eval_is_unbound() {
    let _serial = serial();
    let kernel = kernel();
    assert_eq!(
        eval(&kernel, &lisp_only(), "(define (foo x) x) 0").unwrap(),
        "0"
    );
    let later = eval(
        &kernel,
        &lisp_only(),
        r#"(define a 1) (define b 2) (define c 3) (if foo "foo is bound" "no")"#,
    );
    let err = later.expect_err("an earlier eval's `foo` must be unbound here");
    assert!(
        err.to_string().contains("foo"),
        "the refusal names the identifier: {err}"
    );

    // And a value never crosses: plain defines, closures, vectors. (A struct cannot be
    // planted at all: `struct` needs `make-struct-type`, which no program needs and the
    // sandbox refuses.)
    for (plant, probe) in [
        (r#"(define leaked-value "SECRET-A") 0"#, "leaked-value"),
        (r#"(define (leakf) "SECRET-F") 0"#, "(leakf)"),
        ("(define v (vector 1 2 3)) 0", "v"),
    ] {
        eval(&kernel, &lisp_only(), plant).unwrap_or_else(|e| panic!("{plant}: {e}"));
        let seen = eval(&kernel, &lisp_only(), probe);
        assert!(seen.is_err(), "{probe} after `{plant}` => {seen:?}");
    }
    let planted = eval(&kernel, &lisp_only(), "(struct Pt (x y)) 0");
    assert!(
        planted.is_err(),
        "a struct needs make-struct-type, which the sandbox refuses: {planted:?}"
    );
    let seen = eval(&kernel, &lisp_only(), "(Pt 1 2)");
    assert!(seen.is_err(), "(Pt 1 2) => {seen:?}");
}
