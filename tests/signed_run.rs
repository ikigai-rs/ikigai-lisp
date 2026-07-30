//! `urn:lisp:run` — the signed-program envelope (wire-eval L1.5) — end to end
//! through a real kernel: the program is signed by a genuine `urn:sign:sign`,
//! verified by a genuine `urn:sign:verify` (module composition, keys resolving
//! as kernel resources), and only then evaluated. Own process: the lisp worker
//! pool is a global.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result as CoreResult, Verb,
};

// The same fixed Ed25519 fixtures ikigai-sign's own tests use (PKCS8 private +
// SPKI public, PEM): k1 signs code; k2 is a real key the host does NOT trust.
const K1_PRIV: &str = "-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIEIW/m80W4IrD82k3Mos0l4aeyfOkZMMZXqEYt6jpawc\n\
-----END PRIVATE KEY-----\n";
const K1_PUB: &str = "-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEAa9JuLzyLESJBF9LPZZ4RJk13iu5OhgKvLRQ3q0oQ4pE=\n\
-----END PUBLIC KEY-----\n";
const K2_PUB: &str = "-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEAi+9rQO2fsE5jSht+Wi2itGXQQx/or4ygbZ3CJqIC8wU=\n\
-----END PUBLIC KEY-----\n";

/// Serve fixed bytes as a key resource, so `key=<uri>` resolves through the
/// kernel exactly as in production.
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

/// A kernel composing the lisp space, the REAL sign module, the key resources,
/// and a `urn:lisp:run` that trusts exactly k1.
fn kernel() -> Kernel {
    let keys = EndpointSpace::new()
        .bind(Exact::new("urn:test:k1-priv"), StaticKey(K1_PRIV))
        .bind(Exact::new("urn:test:k1-pub"), StaticKey(K1_PUB))
        .bind(Exact::new("urn:test:k2-pub"), StaticKey(K2_PUB))
        .bind(
            Exact::new("urn:lisp:run"),
            ikigai_lisp::run_signed(["urn:test:k1-pub"]),
        );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(keys),
        Arc::new(ikigai_lisp::space()),
        Arc::new(ikigai_sign::space()),
    ])))
}

/// Sign `program` with `priv_uri` through the real `urn:sign:sign`, returning
/// the RDF signature-graph Turtle.
fn sign(kernel: &Kernel, program: &str, priv_uri: &str) -> String {
    let signer = Capability::root().attenuate(["urn:cap:sign".to_string()]);
    let out = block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:sign:sign").unwrap())
                .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
                .with_arg("key", ArgRef::Inline(priv_uri.as_bytes().to_vec())),
            &signer,
        ),
    )
    .expect("signing succeeds");
    String::from_utf8(out.bytes).expect("signature graph is UTF-8")
}

fn run(program: &str, sig: &str, key: &str) -> Request {
    Request::new(Verb::Source, Iri::parse("urn:lisp:run").unwrap())
        .with_arg("in", ArgRef::Inline(program.as_bytes().to_vec()))
        .with_arg("sig", ArgRef::Inline(sig.as_bytes().to_vec()))
        .with_arg("key", ArgRef::Inline(key.as_bytes().to_vec()))
}

/// A ceiling that may SUBMIT signed programs — but does NOT hold `urn:cap:lisp`
/// (arbitrary eval): the signature is what authorizes the code.
fn submitter() -> Capability {
    Capability::root().attenuate(["urn:cap:lisp:run".to_string()])
}

#[test]
fn a_trusted_signature_runs_and_every_other_path_is_refused() {
    let kernel = kernel();
    let program = "(+ 20 22)";
    let sig = sign(&kernel, program, "urn:test:k1-priv");

    // Happy path: signed by the trusted key → evaluates, under a ceiling that
    // could NOT have called urn:lisp:eval (no urn:cap:lisp).
    let out = block_on(kernel.issue(run(program, &sig, "urn:test:k1-pub"), &submitter()))
        .expect("a trusted signed program runs");
    assert_eq!(out.bytes, b"42");

    // Tampered: one changed byte after signing → typed Denied, nothing evaluates.
    let err = block_on(kernel.issue(run("(+ 20 23)", &sig, "urn:test:k1-pub"), &submitter()))
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
    assert!(
        err.to_string().contains("signature rejected"),
        "the refusal names the cause: {err}"
    );

    // Untrusted key: a key outside the trust set is refused BEFORE verification
    // even runs — trust is the host's declaration, not the signer's. (The k1
    // signature is real; only the presented key is un-enrolled.)
    let err =
        block_on(kernel.issue(run(program, &sig, "urn:test:k2-pub"), &submitter())).unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
    assert!(
        err.to_string().contains("trust set"),
        "the refusal names the trust set: {err}"
    );

    // Missing capability: without urn:cap:lisp:run even a trusted, valid
    // program is refused — at the kernel's declared-requires floor.
    let bystander = Capability::root().attenuate(["urn:cap:other".to_string()]);
    let err =
        block_on(kernel.issue(run(program, &sig, "urn:test:k1-pub"), &bystander)).unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");

    // Data stays data: piped content reaches (input) unsigned; the signature
    // still only covers the program.
    let reader = "(string-append \"got: \" (input))";
    let rsig = sign(&kernel, reader, "urn:test:k1-priv");
    let out = block_on(kernel.issue(
        run(reader, &rsig, "urn:test:k1-pub").with_arg("data", ArgRef::Inline(b"a tuple".to_vec())),
        &submitter(),
    ))
    .expect("a data-reading program runs");
    assert_eq!(out.bytes, b"got: a tuple");
}
