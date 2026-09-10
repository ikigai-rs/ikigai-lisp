//! The module recipe as one test: `ikigai-conformance` walks the three endpoint
//! kinds this crate exports — `urn:lisp:eval` ([`ikigai_lisp::eval`]), a stored
//! program ([`ikigai_lisp::program`]) and the signed-run door
//! ([`ikigai_lisp::run_signed`]) — and reports every violation at once.
//!
//! ## The fixture kernel is a composition, and the walk covers all of it
//!
//! `urn:lisp:run` verifies THROUGH the kernel (`urn:sign:verify`), so the fixture
//! binds the real `ikigai-sign` module and a keypair as kernel resources, and signs
//! the fixture program before the walk. The suite walks everything the kernel
//! binds (conformance PENDING #17), so the sign endpoints get the same fixtures
//! their own suite gives them, and every fixture resource ([`Static`]) describes
//! itself the way a module endpoint must: a kebab-case id, a Source action, an
//! output, and `requires` where a capability gates it.
//!
//! ## Cacheability: an eval is live by design, and the PROGRAM opts in
//!
//! No lisp endpoint is pure and none is declared `cacheable`. A program can call
//! any kernel verb, so an eval's cacheability is whatever its sub-resolutions
//! carry — and an eval that touches nothing is STILL served uncacheable
//! (`Expiry::Always`), because a program that opted into nothing said nothing
//! about its own determinism. The only way an eval is cached is the program's
//! own `(cacheable …)` / `(cacheable/ttl …)` form, honored only when no verb
//! mutated and clamped by the kernel to the eval's inputs. The suite has no
//! spelling for "live on purpose" (PENDING #22), so
//! [`an_eval_is_live_until_its_program_says_otherwise`] pins the decision by
//! hand and then makes the declaration the suite DOES have, `cacheable("eval")`,
//! to show the one finding it costs.
//!
//! ## What the suite cannot see and this file pins by hand
//!
//! - **A gate inside a sub-resolution** (PENDING #21/#29): ENFORCED sees only
//!   `urn:cap:lisp` on the eval itself. A program that reaches a cap-gated
//!   resource under a capability holding `urn:cap:lisp` and nothing else must
//!   come out as THAT resource's typed `Denied` — permanent, not an opaque
//!   `Endpoint` string — on all three doors
//!   ([`a_denied_sub_resolution_leaves_the_program_as_the_same_typed_denied`]).
//! - **Declared outputs are the media types served, both directions, with `as`
//!   omitted** (PENDING #11/#31): the suite compares the two only for RDF faces,
//!   and this module has none ([`declared_outputs_are_the_media_types_served`]).
//! - **`content` is read where it is declared** ([`content_drives_every_door`]):
//!   PIPELINE's invoking half only fires an action that declares `content`, and
//!   only reports a `MissingArgument`; that the piped value actually reaches the
//!   program is asserted here.
//!
//! No opt-outs, and NAMES runs: every id is kebab-case.

use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_conformance::{rdf, Check, Fixture, Report, Suite};
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, Error, Exact, Expiry, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result as CoreResult, Verb,
};
use ikigai_lisp::{CAP_LISP, CAP_LISP_RUN};

/// The description ids the walk sees, pinned against the kernel in
/// [`the_fixture_ids_are_the_description_ids`]: a `Fixture` is matched BY ID and
/// one that drifts is silently never applied.
const EVAL: &str = "eval";
const RUN: &str = "lisp-run";
const ECHO: &str = "echo-prog";
const VAULT_READER: &str = "vault-reader";
const SIGN: &str = "sign";
const VERIFY: &str = "verify";

const EVAL_IRI: &str = "urn:lisp:eval";
const RUN_IRI: &str = "urn:lisp:run";
/// A stored program that echoes its `(input)` — the reactor's shape.
const ECHO_IRI: &str = "urn:conformance:echo";
/// A stored program that reads the gated vault — the sub-resolution gate.
const VAULT_READER_IRI: &str = "urn:conformance:vault-reader";
/// A resource whose Source requires [`CAP_VAULT`]; readable under root.
const VAULT_IRI: &str = "urn:conformance:vault";
const CAP_VAULT: &str = "urn:cap:conformance:vault";
/// A resource served cacheable under a golden thread named after itself.
const THREADED_IRI: &str = "urn:conformance:threaded";
/// The keypair, bound as kernel resources the way `urn:file:<pem>` would be.
const PRIVATE_IRI: &str = "urn:conformance:key:private";
const PUBLIC_IRI: &str = "urn:conformance:key:public";

/// The program every fired action evaluates: pure, so the walk is deterministic.
const PROGRAM: &str = "(+ 40 2)";

/// `openssl genpkey -algorithm ed25519` — the same fixed pair `tests/signed_run.rs`
/// and `ikigai-sign`'s own suite use. PKCS8 private, SPKI public, PEM.
const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIEIW/m80W4IrD82k3Mos0l4aeyfOkZMMZXqEYt6jpawc\n\
-----END PRIVATE KEY-----\n";
const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEAa9JuLzyLESJBF9LPZZ4RJk13iu5OhgKvLRQ3q0oQ4pE=\n\
-----END PUBLIC KEY-----\n";

/// Fixed bytes behind an IRI, described the way a module endpoint must be — the
/// suite walks fixtures beside the module (PENDING #17), so a bare `Endpoint` with
/// the default `describe()` would be a finding of this file's own making.
struct Static {
    id: &'static str,
    media: &'static str,
    body: &'static str,
    /// The capability the Source requires — declared, so the kernel enforces it
    /// before dispatch and ENFORCED sees a typed `Denied` under no grants.
    requires: Option<&'static str>,
    /// The golden thread the bytes are served cacheable under; `None` is live.
    thread: Option<&'static str>,
}

#[async_trait]
impl Endpoint for Static {
    async fn invoke(&self, _inv: &Invocation<'_>) -> CoreResult<Representation> {
        let repr = Representation::new(ReprType::new(self.media), self.body.as_bytes().to_vec());
        Ok(match self.thread {
            Some(thread) => repr.cacheable().depends_on(thread),
            None => repr,
        })
    }

    fn name(&self) -> &str {
        self.id
    }

    fn describe(&self) -> Description {
        let description = Description::new(self.id)
            .title("Conformance fixture resource")
            .summary("Fixed bytes behind an IRI, served as a kernel resource for the walk.")
            .verb(Verb::Source)
            .output(self.media);
        match self.requires {
            Some(cap) => description.requires(cap),
            None => description,
        }
    }
}

/// The fixture kernel: the three lisp doors, the real signing module they compose
/// with, the keypair, and the two resources the hand tests reach for.
fn kernel() -> Kernel {
    let space = ikigai_sign::space()
        .bind(Exact::new(EVAL_IRI), ikigai_lisp::eval())
        .bind(Exact::new(RUN_IRI), ikigai_lisp::run_signed([PUBLIC_IRI]))
        .bind(
            Exact::new(ECHO_IRI),
            ikigai_lisp::program(ECHO, r#"(string-append "got: " (input))"#),
        )
        .bind(
            Exact::new(VAULT_READER_IRI),
            ikigai_lisp::program(VAULT_READER, format!(r#"(source "{VAULT_IRI}")"#)),
        )
        .bind(
            Exact::new(PRIVATE_IRI),
            Static {
                id: "key-private",
                media: "application/x-pem-file",
                body: PRIVATE_PEM,
                requires: None,
                thread: Some(PRIVATE_IRI),
            },
        )
        .bind(
            Exact::new(PUBLIC_IRI),
            Static {
                id: "key-public",
                media: "application/x-pem-file",
                body: PUBLIC_PEM,
                requires: None,
                thread: Some(PUBLIC_IRI),
            },
        )
        .bind(
            Exact::new(VAULT_IRI),
            Static {
                id: "vault",
                media: "text/plain",
                body: "secret",
                requires: Some(CAP_VAULT),
                thread: None,
            },
        )
        .bind(
            Exact::new(THREADED_IRI),
            Static {
                id: "threaded",
                media: "text/plain",
                body: "v1",
                requires: None,
                thread: Some(THREADED_IRI),
            },
        );
    Kernel::new(Arc::new(space))
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("a valid IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn issue(kernel: &Kernel, request: Request, capability: &Capability) -> Representation {
    block_on(kernel.issue(request, capability)).unwrap_or_else(|e| panic!("resolution failed: {e}"))
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("UTF-8")
}

/// Sign `program` through the real `urn:sign:sign`, returning the signature-graph
/// Turtle the run door verifies against.
fn sign(kernel: &Kernel, program: &str) -> String {
    text(&issue(
        kernel,
        request(
            Verb::Source,
            "urn:sign:sign",
            &[("in", program), ("key", PRIVATE_IRI)],
        ),
        &Capability::root(),
    ))
}

/// The eval-only capability: may run Lisp, holds nothing else.
fn lisp() -> Capability {
    Capability::scoped([CAP_LISP])
}

/// The suite, configured for this module (the file docs say why each line): the
/// signing module's namespace for the face the walk crosses, the fixture program
/// where the suite's `x` is not a program, the signed graph where its `x` is not a
/// signature, and the sign endpoints' own fixtures. No `pure`, no `cacheable` on
/// any lisp door; the two sign results are held to the cache over this threaded
/// keystore, as their own suite holds them.
fn suite(graph: &str) -> Suite {
    Suite::new()
        .namespace(ikigai_sign::SIG_NS)
        .fixture(Fixture::new(EVAL, Verb::Source).arg("in", PROGRAM))
        .fixture(
            Fixture::new(RUN, Verb::Source)
                .arg("in", PROGRAM)
                .arg("sig", graph)
                .arg("key", PUBLIC_IRI),
        )
        .fixture(
            Fixture::new(SIGN, Verb::Source)
                .arg("in", PROGRAM)
                .arg("key", PRIVATE_IRI),
        )
        .fixture(
            Fixture::new(VERIFY, Verb::Source)
                .arg("in", PROGRAM)
                .arg("sig", graph)
                .arg("key", PUBLIC_IRI),
        )
        .cacheable(SIGN)
        .cacheable(VERIFY)
}

/// The walk saw the four lisp endpoints, the two sign endpoints and the four
/// fixture resources — one Source action each — and skipped nothing. An endpoint
/// bound without a line here would be held to a weaker standard; a declared id
/// that binds nothing is a stale list.
fn assert_shape(report: &Report) {
    assert_eq!(
        report.endpoints, 10,
        "eval, lisp-run, echo-prog, vault-reader, sign, verify, key-private, key-public, \
         vault, threaded: {report}"
    );
    assert_eq!(report.actions, 10, "one Source action each: {report}");
    let skipped: Vec<Check> = report.checks.skipped().collect();
    assert!(skipped.is_empty(), "every check ran: {report}");
}

#[test]
fn conforms() {
    let kernel = kernel();
    let graph = sign(&kernel, PROGRAM);
    let report = suite(&graph).run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);
}

/// The cacheability decision, in the code's own terms and then in the suite's.
#[test]
fn an_eval_is_live_until_its_program_says_otherwise() {
    let kernel = kernel();
    let cap = lisp();

    // Touches nothing, opts into nothing: uncacheable by design, never stored.
    let plain = request(Verb::Source, EVAL_IRI, &[("in", PROGRAM)]);
    let repr = issue(&kernel, plain.clone(), &cap);
    assert_eq!(text(&repr), "42");
    assert_eq!(repr.expiry, Expiry::Always, "no opt-in: live");
    assert!(
        !kernel.is_cached(&plain, &cap),
        "a live eval is never stored"
    );

    // The program opts in: cached, with an empty thread set — the AUTHOR's promise
    // that the form is a pure function, which is why this file cannot declare the
    // endpoint `pure` (that would be true of this program, not of the endpoint).
    let opted = request(Verb::Source, EVAL_IRI, &[("in", "(cacheable (+ 40 2))")]);
    let repr = issue(&kernel, opted.clone(), &cap);
    assert_eq!(repr.expiry, Expiry::Never);
    assert!(repr.threads().is_empty(), "a pure form carries no thread");
    assert!(kernel.is_cached(&opted, &cap), "the opt-in is stored");

    // Over a threaded resource the opt-in inherits that resource's thread, and a
    // cut invalidates the eval that read it — the kernel's fold, not this module's.
    let over_threaded = request(
        Verb::Source,
        EVAL_IRI,
        &[("in", &format!(r#"(cacheable (source "{THREADED_IRI}"))"#))],
    );
    let repr = issue(&kernel, over_threaded.clone(), &cap);
    assert_eq!(text(&repr), "v1");
    assert!(
        repr.threads().iter().any(|t| t.to_string() == THREADED_IRI),
        "the sourced resource's thread flows onto the eval: {:?}",
        repr.threads()
    );
    assert!(kernel.is_cached(&over_threaded, &cap));
    kernel.cut(THREADED_IRI);
    assert!(
        !kernel.is_cached(&over_threaded, &cap),
        "cutting the sourced thread invalidates the cached eval"
    );

    // The suite's only spelling for the decision is the declaration it would
    // contradict: `cacheable("eval")` over the fixture program costs exactly one
    // finding — CACHEABLE on eval's Source — and nothing else changes.
    let graph = sign(&kernel, PROGRAM);
    let report = suite(&graph).cacheable(EVAL).run_blocking(&kernel);
    eprintln!("[declared cacheable, to show the red line]\n{report}");
    assert_eq!(
        report.findings.len(),
        1,
        "one finding, the declaration itself: {report}"
    );
    let finding = &report.findings[0];
    assert_eq!(finding.endpoint, EVAL);
    assert_eq!(finding.verb, Some(Verb::Source));
    assert_eq!(finding.check, Check::Cacheable);
    assert!(
        finding.detail.contains("declared cacheable"),
        "the finding names the declaration: {finding}"
    );
}

/// The gate the suite cannot reach (PENDING #21/#29): a program that touches a
/// cap-gated resource under a capability that may run Lisp but holds no grant on
/// that resource. The resource's `Denied` is raised inside the program as a
/// catchable error; uncaught, it leaves the door as the SAME typed, permanent
/// `Denied` — not an `Endpoint` string a retry overlay or an agent would have to
/// sniff — on every door a program runs behind.
#[test]
fn a_denied_sub_resolution_leaves_the_program_as_the_same_typed_denied() {
    let kernel = kernel();
    let reader = format!(r#"(source "{VAULT_IRI}")"#);

    // Through urn:lisp:eval, under urn:cap:lisp alone.
    let err = block_on(kernel.issue(request(Verb::Source, EVAL_IRI, &[("in", &reader)]), &lisp()))
        .expect_err("the vault is gated");
    assert!(matches!(err, Error::Denied(_)), "eval: got {err:?}");
    assert!(!err.is_transient(), "a denial is permanent");
    assert!(
        err.to_string().contains(CAP_VAULT),
        "the denial is the VAULT's, naming its capability: {err}"
    );

    // Caught by the program, it is an ordinary value — the door succeeds.
    let caught = issue(
        &kernel,
        request(
            Verb::Source,
            EVAL_IRI,
            &[(
                "in",
                &format!(r#"(with-handler (lambda (e) "caught") {reader})"#),
            )],
        ),
        &lisp(),
    );
    assert_eq!(text(&caught), "caught");

    // Through a stored program.
    let err = block_on(kernel.issue(request(Verb::Source, VAULT_READER_IRI, &[]), &lisp()))
        .expect_err("the stored program reaches the gated vault");
    assert!(matches!(err, Error::Denied(_)), "program: got {err:?}");
    assert!(err.to_string().contains(CAP_VAULT), "program: {err}");

    // Through the signed-run door, under urn:cap:lisp:run alone: the signature
    // admits the code, and the session capability still bounds what it touches.
    let graph = sign(&kernel, &reader);
    let err = block_on(kernel.issue(
        request(
            Verb::Source,
            RUN_IRI,
            &[("in", &reader), ("sig", &graph), ("key", PUBLIC_IRI)],
        ),
        &Capability::scoped([CAP_LISP_RUN]),
    ))
    .expect_err("a signed program is still bounded by the session capability");
    assert!(matches!(err, Error::Denied(_)), "run: got {err:?}");
    assert!(
        err.to_string().contains(CAP_VAULT),
        "the denial is the vault's, not the run door's own gate: {err}"
    );

    // And with the grant, the same program reads the vault.
    let repr = issue(
        &kernel,
        request(Verb::Source, EVAL_IRI, &[("in", &reader)]),
        &Capability::scoped([CAP_LISP, CAP_VAULT]),
    );
    assert_eq!(text(&repr), "secret");
}

/// The suite compares served and declared media types only for RDF faces
/// (PENDING #11/#31); this module serves text. With `as` omitted, every door
/// serves one of its declared outputs; with `as=<declared>`, it serves that one.
#[test]
fn declared_outputs_are_the_media_types_served() {
    let kernel = kernel();
    let graph = sign(&kernel, PROGRAM);
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (EVAL_IRI, vec![("in", PROGRAM)]),
        (
            RUN_IRI,
            vec![("in", PROGRAM), ("sig", &graph), ("key", PUBLIC_IRI)],
        ),
        (ECHO_IRI, vec![]),
        (VAULT_READER_IRI, vec![]),
    ];
    for (iri, args) in cases {
        let description = kernel
            .describe(&Iri::parse(iri).unwrap())
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        let spec = description
            .action_specs()
            .into_iter()
            .find(|a| a.verb == Verb::Source)
            .unwrap_or_else(|| panic!("{iri} declares Source"));
        let declared: Vec<String> = spec
            .outputs
            .iter()
            .map(|o| rdf::bare_media_type(o))
            .collect();
        assert!(!declared.is_empty(), "{iri} declares an output");

        let served = issue(
            &kernel,
            request(Verb::Source, iri, &args),
            &Capability::root(),
        );
        let got = rdf::bare_media_type(&served.repr_type.media_type);
        assert!(
            declared.contains(&got),
            "{iri} served `{got}` with `as` omitted, declared only {declared:?}"
        );
        for output in &declared {
            let mut with_as = args.clone();
            with_as.push(("as", output));
            let served = issue(
                &kernel,
                request(Verb::Source, iri, &with_as),
                &Capability::root(),
            );
            assert_eq!(
                &rdf::bare_media_type(&served.repr_type.media_type),
                output,
                "{iri} as={output}"
            );
        }
    }
}

/// `content` is declared on every door and read where it is declared: the
/// s-expression for `urn:lisp:eval`, the `(input)` data for a stored program and
/// for the signed-run door. PIPELINE fires each with `content=x` and reports only
/// a `MissingArgument`; that the value ARRIVES is pinned here.
#[test]
fn content_drives_every_door() {
    let kernel = kernel();
    for iri in [EVAL_IRI, RUN_IRI, ECHO_IRI] {
        let description = kernel
            .describe(&Iri::parse(iri).unwrap())
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        let content = description
            .inputs
            .iter()
            .find(|i| i.name == "content")
            .unwrap_or_else(|| panic!("{iri} declares `content`"));
        assert!(
            !content.required,
            "{iri}: `content` is the piped alternative"
        );
    }

    // The eval: piped content IS the program when `in` is absent.
    let repr = issue(
        &kernel,
        request(Verb::Source, EVAL_IRI, &[("content", PROGRAM)]),
        &lisp(),
    );
    assert_eq!(text(&repr), "42");

    // A stored program: piped content is its `(input)` — data, never code.
    let repr = issue(
        &kernel,
        request(Verb::Source, ECHO_IRI, &[("content", "(+ 1 1)")]),
        &lisp(),
    );
    assert_eq!(text(&repr), "got: (+ 1 1)", "data is not evaluated");

    // The signed-run door: piped content is the program's unsigned `(input)`.
    let program = r#"(string-append "input: " (input))"#;
    let graph = sign(&kernel, program);
    let repr = issue(
        &kernel,
        request(
            Verb::Source,
            RUN_IRI,
            &[
                ("in", program),
                ("sig", &graph),
                ("key", PUBLIC_IRI),
                ("content", "a tuple"),
            ],
        ),
        &Capability::scoped([CAP_LISP_RUN]),
    );
    assert_eq!(text(&repr), "input: a tuple");
}

/// `Fixture::new(id, …)` is looked up by description id, and an id that matches
/// no description is not an error — the fixture is silently unused and the action
/// runs with the derived minimal inputs instead. So the ids this file uses are held
/// to what the kernel serves.
#[test]
fn the_fixture_ids_are_the_description_ids() {
    let kernel = kernel();
    for (iri, id) in [
        (EVAL_IRI, EVAL),
        (RUN_IRI, RUN),
        (ECHO_IRI, ECHO),
        (VAULT_READER_IRI, VAULT_READER),
        ("urn:sign:sign", SIGN),
        ("urn:sign:verify", VERIFY),
    ] {
        let description = kernel
            .describe(&Iri::parse(iri).unwrap())
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        assert_eq!(description.id, id, "{iri}");
    }
}
