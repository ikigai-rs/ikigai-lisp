//! `ikigai-lisp` — a Steel-backed Lisp evaluator whose builtins ARE the kernel
//! verbs, cap-scoped.
//!
//! A standalone **ikigai module crate** (like `ikigai-fn` / `ikigai-text`): a host
//! links it in and mounts [`space`], rather than the engine shipping the behaviour
//! itself. It depends only on the published `ikigai-core` kernel.
//!
//! One endpoint, [`urn:lisp:eval`](eval), evaluates an s-expression with
//! [Steel](https://github.com/mattwparas/steel). It is the reference for a single
//! idea: **code is a resource, and the capability is the builtin set**. The Lisp
//! program's primitives are the five kernel verbs —
//!
//! - `(source "urn:…" [input])` — SOURCE a resource (optional positional input,
//!   routed to the target's conventional `in` argument);
//! - `(sink "urn:…" content)` — SINK `content` to a resource;
//! - `(meta "urn:…" [type])` — the target's self-description (default `text/turtle`);
//! - `(exists "urn:…")` / `(delete "urn:…")` — the existence and delete verbs.
//!
//! Every one issues a **sub-request back through the host kernel**, carrying the
//! eval's own [`Capability`](ikigai_core::Capability) — never touching the
//! filesystem or network directly. So the same capability that gates *running Lisp
//! at all* (the `urn:cap:lisp` grant this endpoint requires) also attenuates every
//! verb the program can reach: a `(sink …)` the capability doesn't authorize comes
//! back as a typed [`Denied`](ikigai_core::Error::Denied), surfaced to the program
//! as a **catchable Steel error** (`with-handler`), never a panic.
//!
//! ## The two capability layers
//!
//! 1. **`urn:cap:lisp`** gates "may run arbitrary Lisp at all." It is declared on
//!    the eval action's `requires` and enforced at entry (an eval without it is
//!    denied before a single form is read).
//! 2. **Per-verb enforcement.** Each verb sub-request is checked by the carried
//!    capability through the ordinary kernel machinery — the target endpoint's own
//!    ACL. Slice 1 enforces the capability *on every sub-request*; binding only the
//!    builtins a capability authorizes (the true manifold-under-cap projection) is a
//!    deliberate follow-up, not built here.
//!
//! ## How the synchronous evaluator reaches the async kernel
//!
//! Steel evaluates **synchronously** on its own thread, while the kernel issues
//! sub-requests **asynchronously**. The two are bridged by a channel: a builtin
//! sends its verb/target over a channel and parks on the reply; the async
//! `invoke` task services each request by issuing it through the kernel (`await`)
//! and sends the representation back. This keeps the module free of any nested
//! executor and lets `register_fn`'s `Send + Sync + 'static` closures avoid
//! borrowing the (non-`'static`) invocation.
//!
//! Because an eval may `sink`/mutate, its result is **uncacheable**.
//!
//! ## Amortizing the engine (warm-clone pool)
//!
//! Building a full Steel VM (`Engine::new_sandboxed`, stdlib + no-dylib posture)
//! costs ~50 ms; a kernel verb call costs ~1 ms. So the VM is built **once per
//! worker** and each eval runs on a cheap **clone of that warm template**
//! (~0.2 ms). Steel's `Engine` is `!Send` (it holds `Rc`s), so a template is
//! pinned to the thread that built it: the module keeps a small pool of
//! **worker threads**, each owning one warm template, and checks one out per
//! eval (spawning a fresh one only when all are busy — so nested and concurrent
//! evals never block each other). A worker clones its template, runs the user
//! program on the clone, and drops the clone; the template is never mutated.
//!
//! **Isolation** is the crux (`urn:lisp:eval` is stateless — two unrelated
//! evals, e.g. future wire-eval from different peers, must not share globals). A
//! clone deep-copies the global environment, so a `(define x 5)` lands only in
//! that clone and vanishes when it is dropped — the next eval clones a pristine
//! template. (Steel's `GlobalCheckpoint`/`rollback_to_checkpoint` was measured
//! *faster* but does **not** isolate — a rolled-back binding is still readable —
//! so clone-per-eval is used.) The `PRELUDE` runs once on the template, so its
//! definitions survive into every clone while user state does not.
//!
//! Verb builtins are registered once on the template and reused by every clone,
//! so they cannot capture a per-eval channel. Instead they read the **current
//! eval's servicing channel from a thread-local** ([`CURRENT_TX`]), which the
//! worker sets before each run and clears after. The eval's [`Capability`] is
//! never on the worker at all: sub-requests are serviced back on the async
//! `invoke` side under *that* invocation's capability, so per-eval attenuation
//! is preserved unchanged.

use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, Verb,
};
use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

/// The capability gating "may run arbitrary Lisp at all." Declared on the eval
/// action's `requires` and enforced at entry.
pub const CAP_LISP: &str = "urn:cap:lisp";

/// The one dialect available in slice 1. `dialect` leaves room for `elisp`/`cl`.
const DIALECT_STEEL: &str = "steel";

/// The XSD `string` datatype IRI — the `class` of the s-expression input.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The conventional `text/plain; charset=utf-8` representation type.
fn text_plain_utf8() -> ReprType {
    ReprType::new("text/plain").with_param("charset", "utf-8")
}

/// The same media type as a description-output string.
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";

/// The Scheme surface: friendly, variadic verb wrappers over the fixed-arity
/// `%`-prefixed primitives the Rust side registers. Steel's raw variadic-function
/// registration is not public API, so optionality is expressed here, in Scheme,
/// where rest-arguments are trivial (`(source iri)` and `(source iri input)` both
/// work). Run once per engine before the user program.
const PRELUDE: &str = r#"
(define (source iri . rest)
  (if (null? rest) (%source iri) (%source-in iri (car rest))))
(define (sink iri content) (%sink iri content))
(define (meta iri . rest)
  (if (null? rest) (%meta iri) (%meta-as iri (car rest))))
(define (exists iri) (%exists iri))
(define (delete iri) (%delete iri))
"#;

/// The `urn:lisp:eval` endpoint: evaluate an s-expression whose builtins are the
/// cap-scoped kernel verbs. See the [module docs](crate).
pub struct LispEval;

/// Construct the [`urn:lisp:eval`](LispEval) endpoint.
pub fn eval() -> LispEval {
    LispEval
}

/// Mount the module at its conventional IRI (`urn:lisp:eval`). A host links this
/// crate and mounts the returned space; the running principal's
/// [`Capability`](ikigai_core::Capability) then gates both the eval itself and
/// every verb the program reaches.
pub fn space() -> EndpointSpace {
    EndpointSpace::new().bind(Exact::new("urn:lisp:eval"), eval())
}

#[async_trait]
impl Endpoint for LispEval {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // Layer 1 — enforce `urn:cap:lisp` at entry. `requires` is descriptive
        // (drives catalog projection); the kernel does not enforce it for bound
        // endpoints, so — like `ikigai-fs`'s path-ACL — this endpoint enforces its
        // declared authority itself. A typed `Denied` (permanent, never transient).
        if !inv.capability.allows(CAP_LISP) {
            return Err(Error::Denied(format!(
                "urn:lisp:eval requires the {CAP_LISP} capability"
            )));
        }

        // The dialect is fixed to `steel` in slice 1; reject anything else rather
        // than silently evaluating it as Steel.
        let dialect = inv.inline_str("dialect").unwrap_or(DIALECT_STEEL).trim();
        if dialect != DIALECT_STEEL {
            return Err(Error::InvalidArgument {
                name: "dialect".to_string(),
                detail: format!(
                    "unsupported dialect {dialect:?}; only `{DIALECT_STEEL}` is available"
                ),
            });
        }

        let src = read_source(inv)?.to_string();

        // Bridge the synchronous evaluator (a pooled worker thread with a warm
        // engine) to the async kernel: a builtin sends a `VerbCall` and parks; we
        // service each by issuing it, so the sub-request carries this invocation's
        // capability and its golden threads fold into the result. `call_tx` is the
        // only sender for this eval — the worker drops it when the run finishes,
        // which is how the servicing loop below learns the eval is done.
        let (call_tx, call_rx) = crossbeam_channel::unbounded::<VerbCall>();
        let (result_tx, result_rx) = crossbeam_channel::bounded::<Result<String>>(1);

        // Check out a warm worker (or spawn one if all are busy). Sending the job
        // cannot fail: a pooled worker is alive (its `job_rx` sender is held here),
        // and a freshly spawned one is too.
        let worker = checkout_worker();
        let sent = worker.send(EvalJob {
            src,
            call_tx,
            result_tx,
        });

        if sent.is_ok() {
            // `recv` returns `Err` once the worker finishes the run and drops the
            // eval's `call_tx` — the loop's natural exit.
            while let Ok(call) = call_rx.recv() {
                let result = dispatch(inv, &call).await;
                // The evaluator parked on this reply; a send failure only means it
                // went away first, which the next `recv` will observe.
                let _ = call.reply.send(result);
            }
        }

        // A live worker sends exactly one result, then is safe to reuse. A worker
        // that died mid-run (a Steel panic) drops `result_tx`, so `recv` errors —
        // surface it as an endpoint error and let that worker go (do not re-pool).
        match result_rx.recv() {
            Ok(text) => {
                check_in_worker(worker);
                // Uncacheable: an eval may sink/mutate. (No `.cacheable()`.)
                Ok(Representation::new(text_plain_utf8(), text?.into_bytes()))
            }
            Err(_) => Err(Error::Endpoint(
                "lisp: evaluator thread panicked".to_string(),
            )),
        }
    }

    fn name(&self) -> &str {
        "eval"
    }

    fn describe(&self) -> Description {
        Description::new("eval")
            .title("Lisp eval")
            .summary(
                "Evaluates an s-expression with the Steel Scheme engine. Its builtins are the \
                 cap-scoped kernel verbs — `(source iri [input])`, `(sink iri content)`, \
                 `(meta iri [type])`, `(exists iri)`, `(delete iri)` — each issued back through \
                 the kernel carrying this eval's capability, so a denied verb surfaces as a \
                 catchable Steel error (`with-handler`), never a panic. Requires `urn:cap:lisp`; \
                 the result is the last form's value as text. Uncacheable (an eval may mutate).",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_LISP)
            .input(
                ArgSpec::new("in")
                    .summary("the s-expression source to evaluate (piped/positional)")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("dialect")
                    .summary("the Lisp dialect; only `steel` is available in slice 1")
                    .one_of([DIALECT_STEEL])
                    .default_value(DIALECT_STEEL),
            )
            .output(TEXT_PLAIN_UTF8)
    }
}

/// The s-expression source: the `in` argument, falling back to a piped `content`
/// (pipeline citizenship — a stage piped into `urn:lisp:eval` arrives as `content`).
fn read_source<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    match inv.inline_str("in") {
        Ok(src) => Ok(src),
        Err(_) => inv.inline_str("content"),
    }
}

/// One verb invocation the Steel side asks the kernel to perform, with a reply
/// channel the builtin parks on. `input` fills a Source target's conventional `in`;
/// `content` is a Sink body; `as_type` is a Meta representation request.
struct VerbCall {
    verb: Verb,
    iri: String,
    input: Option<String>,
    content: Option<String>,
    as_type: Option<String>,
    reply: Sender<std::result::Result<String, String>>,
}

/// Service one `VerbCall` by issuing it through the kernel under this invocation's
/// capability, returning the representation's text or a message. A kernel error
/// (including a typed `Denied`) becomes the `Err` string the builtin re-raises as a
/// catchable Steel error.
async fn dispatch(inv: &Invocation<'_>, call: &VerbCall) -> std::result::Result<String, String> {
    let iri =
        Iri::parse(&call.iri).map_err(|e| format!("lisp: invalid IRI `{}`: {e}", call.iri))?;
    let mut request = Request::new(call.verb, iri);
    if let Some(input) = &call.input {
        request = request.with_arg("in", ArgRef::Inline(input.clone().into_bytes()));
    }
    if let Some(content) = &call.content {
        request = request.with_arg("content", ArgRef::Inline(content.clone().into_bytes()));
    }
    if let Some(as_type) = &call.as_type {
        request = request.with_arg("as", ArgRef::Inline(as_type.clone().into_bytes()));
    }
    match inv.issue(request).await {
        Ok(repr) => String::from_utf8(repr.bytes)
            .map_err(|_| format!("lisp: `{}` returned non-UTF-8 bytes", call.iri)),
        Err(e) => Err(format!("{e}")),
    }
}

/// One unit of work for a warm worker: source to evaluate, the eval's servicing
/// channel (which the worker installs as [`CURRENT_TX`] for the run), and a
/// one-shot channel to return the rendered result (or a lisp/eval error).
struct EvalJob {
    src: String,
    call_tx: Sender<VerbCall>,
    result_tx: Sender<Result<String>>,
}

thread_local! {
    /// The servicing channel of the eval currently running on THIS worker thread.
    /// The worker sets it before each run and takes (drops) it after; the verb
    /// builtins read it at call time. A worker runs one eval at a time, so this is
    /// unambiguous — and it is what lets the builtins be registered once on a
    /// shared warm template yet route to the right (per-eval) servicing loop.
    static CURRENT_TX: RefCell<Option<Sender<VerbCall>>> = const { RefCell::new(None) };
}

/// The pool of idle warm workers (each an alive thread owning one warm engine,
/// addressed by its job sender). A busy worker is simply absent from the pool;
/// [`checkout_worker`] spawns a new one when none are idle, so nested and
/// concurrent evals never block one another.
static POOL: OnceLock<Mutex<Vec<Sender<EvalJob>>>> = OnceLock::new();

/// Take an idle worker, or spawn a fresh warm one. The returned sender is the
/// handle used to submit a job and (via [`check_in_worker`]) to return the worker.
fn checkout_worker() -> Sender<EvalJob> {
    let pool = POOL.get_or_init(|| Mutex::new(Vec::new()));
    if let Some(worker) = pool.lock().unwrap().pop() {
        return worker;
    }
    // All warm workers are busy (or this is the first eval): spawn a new one. It
    // builds its template once, then serves jobs until its `job_rx` sender is
    // dropped (i.e. the worker is dropped instead of re-pooled).
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<EvalJob>();
    std::thread::spawn(move || worker_loop(job_rx));
    job_tx
}

/// Return a worker to the idle pool for reuse. Only ever called for a worker that
/// completed a run cleanly (a panicked worker is dropped, not re-pooled).
fn check_in_worker(worker: Sender<EvalJob>) {
    if let Some(pool) = POOL.get() {
        pool.lock().unwrap().push(worker);
    }
}

/// A warm worker: build the template engine ONCE, then serve each job on a fresh
/// clone. The template is never mutated (only cloned), so every eval starts from
/// the same pristine post-prelude state — this is what gives per-eval isolation.
fn worker_loop(job_rx: Receiver<EvalJob>) {
    let template = build_template();
    while let Ok(job) = job_rx.recv() {
        // Install this eval's servicing channel for the builtins to reach.
        CURRENT_TX.with(|slot| *slot.borrow_mut() = Some(job.call_tx));
        let mut engine = template.clone();
        let result = run_program(&mut engine, job.src);
        // Drop the eval's `call_tx` so the async servicing loop exits, THEN hand
        // back the result. (Dropping the clone here also releases its state.)
        CURRENT_TX.with(|slot| *slot.borrow_mut() = None);
        let _ = job.result_tx.send(result);
    }
}

/// Build the warm template: a sandboxed engine (full stdlib, dylib loading
/// blocked), the verb primitives registered, and the Scheme [`PRELUDE`] run once.
/// The prelude's definitions live in the template and so survive into every clone.
fn build_template() -> Engine {
    let mut engine = Engine::new_sandboxed();
    register_primitives(&mut engine);
    engine
        .run(PRELUDE)
        .expect("lisp: prelude is a constant and must compile");
    engine
}

/// Run `src` on `engine` (a fresh clone) and render its last value. Returns the
/// rendered text, or a lisp/eval error.
fn run_program(engine: &mut Engine, src: String) -> Result<String> {
    let values = engine
        .run(src)
        .map_err(|e| Error::Endpoint(format!("lisp: {e}")))?;
    Ok(values.last().map(render_value).unwrap_or_default())
}

/// Render a Steel value as text: a string yields its raw contents (no reader
/// quotes), everything else its `Display` (`3`, `#t`, `'(1 2)`, …).
fn render_value(value: &SteelVal) -> String {
    match value {
        SteelVal::StringV(s) => s.to_string(),
        other => other.to_string(),
    }
}

/// Register the fixed-arity verb primitives the Scheme [`PRELUDE`] wraps. Each
/// closure is `Send + Sync + 'static` (it captures nothing), reads the current
/// eval's servicing channel from [`CURRENT_TX`], sends a `VerbCall`, and parks on
/// the reply — an `Err` reply becomes a catchable Steel error via Steel's `Result`
/// conversion. Registered once on the template and reused by every clone.
fn register_primitives(engine: &mut Engine) {
    engine.register_fn("%source", |iri: String| {
        call(Verb::Source, iri, None, None, None)
    });
    engine.register_fn("%source-in", |iri: String, input: String| {
        call(Verb::Source, iri, Some(input), None, None)
    });
    engine.register_fn("%sink", |iri: String, content: String| {
        call(Verb::Sink, iri, None, Some(content), None)
    });
    engine.register_fn("%meta", |iri: String| {
        call(Verb::Meta, iri, None, None, Some("text/turtle".to_string()))
    });
    engine.register_fn("%meta-as", |iri: String, as_type: String| {
        call(Verb::Meta, iri, None, None, Some(as_type))
    });
    engine.register_fn("%exists", |iri: String| {
        call(Verb::Exists, iri, None, None, None)
    });
    engine.register_fn("%delete", |iri: String| {
        call(Verb::Delete, iri, None, None, None)
    });
}

/// Send a `VerbCall` to the current eval's servicing loop and block until the
/// reply — the synchronous face the Steel builtins call. The servicing channel is
/// read from [`CURRENT_TX`], so a builtin registered once on the shared template
/// reaches whichever eval is running on this worker.
fn call(
    verb: Verb,
    iri: String,
    input: Option<String>,
    content: Option<String>,
    as_type: Option<String>,
) -> std::result::Result<String, String> {
    let tx = CURRENT_TX
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| "lisp: no active eval context".to_string())?;
    let (reply, reply_rx) = crossbeam_channel::bounded(1);
    tx.send(VerbCall {
        verb,
        iri,
        input,
        content,
        as_type,
        reply,
    })
    .map_err(|_| "lisp: kernel channel closed".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "lisp: kernel dropped the reply".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, FnEndpoint, Kernel, Result as CoreResult};
    use std::sync::Arc;

    /// A stub Sink that denies unless the capability grants `urn:cap:test:write` —
    /// a stand-in for a real cap-gated write target, so the "denied sink" oracle
    /// exercises the genuine `Denied → catchable Steel error` path.
    struct DenyingVault;

    #[async_trait]
    impl Endpoint for DenyingVault {
        async fn invoke(&self, inv: &Invocation<'_>) -> CoreResult<Representation> {
            if inv.request.verb == Verb::Sink && !inv.capability.allows("urn:cap:test:write") {
                return Err(Error::Denied("vault: no write grant".to_string()));
            }
            Ok(Representation::new(text_plain_utf8(), b"stored".to_vec()))
        }
    }

    /// A kernel that binds the real `ikigai-fn` module (for `urn:fn:toUpper`), the
    /// Lisp eval endpoint, and the denying vault.
    fn kernel() -> Kernel {
        // A no-input constant resource, to exercise single-argument `(source iri)`.
        let ping = FnEndpoint::new("ping", |_inv: &Invocation<'_>| {
            Ok(Representation::new(text_plain_utf8(), b"pong".to_vec()))
        });
        let space = ikigai_fn::space()
            .bind(Exact::new("urn:lisp:eval"), eval())
            .bind(Exact::new("urn:test:vault"), DenyingVault)
            .bind(Exact::new("urn:test:ping"), ping);
        Kernel::new(Arc::new(space))
    }

    fn lisp_cap() -> Capability {
        Capability::scoped(["urn:cap:lisp"])
    }

    /// Evaluate `src` (passed as `in`) under `cap`, returning the whole `Result`.
    fn try_eval(cap: &Capability, src: &str) -> CoreResult<Representation> {
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()));
        block_on(kernel().issue(request, cap))
    }

    /// Evaluate and return the body text (panicking on error).
    fn eval_ok(cap: &Capability, src: &str) -> String {
        String::from_utf8(try_eval(cap, src).unwrap().bytes).unwrap()
    }

    // ---- the five oracle tests --------------------------------------------

    #[test]
    fn pure_sexpr_evaluates() {
        // The evaluator works: a pure form with no verbs.
        assert_eq!(eval_ok(&lisp_cap(), "(+ 1 2)"), "3");
    }

    #[test]
    fn verb_as_function_through_the_kernel() {
        // `(source iri input)` issues a Source sub-request to a REAL sibling module
        // (ikigai-fn) — verb-as-function across a genuine module boundary.
        assert_eq!(
            eval_ok(&lisp_cap(), r#"(source "urn:fn:toUpper" "hi")"#),
            "HI"
        );
    }

    #[test]
    fn eval_without_cap_is_denied_at_entry() {
        let err = try_eval(&Capability::scoped(Vec::<String>::new()), "(+ 1 2)").unwrap_err();
        // A permanent, typed capability denial — not a generic endpoint string.
        assert!(matches!(err, Error::Denied(_)));
        assert!(!err.is_transient());
    }

    #[test]
    fn denied_sink_is_caught_in_lisp_without_panic() {
        // The vault denies the write (the eval cap lacks `urn:cap:test:write`); the
        // typed `Denied` surfaces as a catchable Steel error, trapped by
        // `with-handler` — so the program recovers and returns a value, no panic.
        assert_eq!(
            eval_ok(
                &lisp_cap(),
                r#"(with-handler (lambda (err) "caught") (sink "urn:test:vault" "data"))"#,
            ),
            "caught"
        );
    }

    #[test]
    fn piped_content_fills_the_source_input() {
        // Pipeline citizenship: a value piped into `urn:lisp:eval` arrives as
        // `content` and is read as the s-expression when `in` is absent.
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("content", ArgRef::Inline(b"(+ 40 2)".to_vec()));
        let rep = block_on(kernel().issue(request, &lisp_cap())).unwrap();
        assert_eq!(String::from_utf8(rep.bytes).unwrap(), "42");
    }

    // ---- supporting behaviour ---------------------------------------------

    #[test]
    fn an_uncaught_denial_surfaces_as_an_error_not_a_panic() {
        // Without `with-handler`, the denied sink propagates out of the eval as a
        // Rust `Err` — proving the denial is a real (catchable) error, never a panic.
        assert!(try_eval(&lisp_cap(), r#"(sink "urn:test:vault" "data")"#).is_err());
    }

    #[test]
    fn source_with_no_input_is_supported() {
        // The friendly `source` wrapper is variadic: one argument reads a resource
        // with no input (here a constant), proving `(source iri)` as well as
        // `(source iri input)`.
        assert_eq!(eval_ok(&lisp_cap(), r#"(source "urn:test:ping")"#), "pong");
    }

    #[test]
    fn explicit_steel_dialect_is_accepted() {
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("in", ArgRef::Inline(b"(* 6 7)".to_vec()))
            .with_arg("dialect", ArgRef::Inline(b"steel".to_vec()));
        let rep = block_on(kernel().issue(request, &lisp_cap())).unwrap();
        assert_eq!(String::from_utf8(rep.bytes).unwrap(), "42");
    }

    #[test]
    fn an_unknown_dialect_is_rejected() {
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("in", ArgRef::Inline(b"(+ 1 2)".to_vec()))
            .with_arg("dialect", ArgRef::Inline(b"elisp".to_vec()));
        let err = block_on(kernel().issue(request, &lisp_cap())).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }));
    }

    // ---- describe: the ArgSpec contract -----------------------------------

    #[test]
    fn describe_declares_the_argspec_and_required_capability() {
        let d = eval().describe();
        assert!(d.verbs.contains(&Verb::Source));
        assert!(d.verbs.contains(&Verb::Meta));
        assert_eq!(d.requires, vec![CAP_LISP.to_string()]);

        // `in` is the sole REQUIRED input — so the engine routes a piped/positional
        // value to it (pipeline citizenship) — typed xsd:string.
        let in_arg = d.inputs.iter().find(|a| a.name == "in").expect("`in`");
        assert!(in_arg.required);
        assert_eq!(in_arg.class.as_deref(), Some(XSD_STRING));
        let required: Vec<&str> = d
            .inputs
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(required, vec!["in"], "exactly one required input");

        // `dialect` is an optional enum defaulting to steel.
        let dialect = d
            .inputs
            .iter()
            .find(|a| a.name == "dialect")
            .expect("`dialect`");
        assert!(!dialect.required);
        assert_eq!(dialect.one_of, vec![DIALECT_STEEL]);
        assert_eq!(dialect.default.as_deref(), Some(DIALECT_STEEL));

        // The Source action synthesized from the flat description carries the cap.
        let source = d
            .action_specs()
            .into_iter()
            .find(|a| a.verb == Verb::Source)
            .expect("Source action");
        assert_eq!(source.requires, vec![CAP_LISP.to_string()]);
    }

    // ---- amortized-engine isolation & stdlib ------------------------------

    /// Assert that a bare identifier does NOT resolve to `forbidden` in a fresh
    /// eval — i.e. a prior eval's `(define …)` did not leak. An unbound reference
    /// is correct isolation whether Steel renders it as void (empty string) or
    /// raises a free-identifier error; only the *leaked value* would be a bug.
    fn assert_unbound(cap: &Capability, ident: &str, forbidden: &str) {
        // An `Err` (free-identifier error) is also correct isolation; only an `Ok`
        // rendering the forbidden value would be a leak.
        if let Ok(rep) = try_eval(cap, ident) {
            assert_ne!(
                String::from_utf8(rep.bytes).unwrap(),
                forbidden,
                "`{ident}` leaked the value from a previous eval"
            );
        }
    }

    #[test]
    fn a_define_does_not_leak_into_the_next_eval() {
        // The crux of the warm-clone pool: each eval runs on a fresh clone of the
        // template, so a global defined in one eval is invisible to the next. If a
        // single shared engine (or a leaky rollback) were used, eval 2 would see 5.
        let cap = lisp_cap();
        assert_eq!(eval_ok(&cap, "(define leaked 5) leaked"), "5");
        // A second eval that references `leaked` must NOT get 5.
        assert_unbound(&cap, "leaked", "5");
        // And the prelude's verbs survive into every clone (not rolled away).
        assert_eq!(eval_ok(&cap, r#"(source "urn:test:ping")"#), "pong");
    }

    #[test]
    fn stdlib_higher_order_functions_work() {
        // The warm template is `new_sandboxed` (full stdlib), so `map`/`lambda`
        // remain available — the amortization did not drop the prelude.
        assert_eq!(
            eval_ok(&lisp_cap(), "(map (lambda (x) (* x x)) (list 1 2 3))"),
            "(1 4 9)"
        );
    }

    #[test]
    fn many_sequential_evals_reuse_the_pool_correctly() {
        // Exercise pooled-worker reuse: repeated evals through the same warm worker
        // must each be correct and isolated (no accumulated state).
        let cap = lisp_cap();
        for i in 0..25 {
            assert_eq!(
                eval_ok(&cap, &format!("(define n {i}) (* n 2)")),
                format!("{}", i * 2)
            );
        }
        // After 25 defines of `n`, a bare `n` is still unbound (never leaked the
        // last value, 24).
        assert_unbound(&cap, "n", "24");
    }

    #[test]
    fn eval_result_is_uncacheable() {
        use ikigai_core::Expiry;
        // An eval may mutate — its representation must never be cached.
        let rep = try_eval(&lisp_cap(), "(+ 1 2)").unwrap();
        assert_eq!(rep.expiry, Expiry::Always);
    }
}
