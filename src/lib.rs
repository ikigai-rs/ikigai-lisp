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
//! ## Homoiconic SPARQL — `(sparql-select …)`
//!
//! One builtin goes beyond the raw verbs: `(sparql-select QUERY-SEXPR)` takes a
//! **quoted s-expression** describing a SELECT and runs it. The query is *composed,
//! not string-built* — a Lisp list IS the query AST, so it splices with quasiquote
//! (`` `(select ,vars (where ,@patterns)) ``) and never risks unsanitized
//! interpolation. A pure, kernel-free compiler
//! ([`sexpr_to_sparql`]) turns the s-expr into a SPARQL string; the builtin then
//! issues it as an ordinary Source sub-request to `urn:sparql:select` (`query=`) —
//! the SAME cap-scoped, composable path every verb uses. Grammar:
//!
//! ```text
//! (select (?a ?b …) | *          ; projection: a var list, or * for all
//!   (prefix (rdf "http://…") …)   ; optional PREFIX lines
//!   (where (S P O) …)             ; triple patterns; S/P/O = ?var | (iri "…")
//!                                 ;   | pfx:local | "string" | number | a
//!   (order-by ?v (desc ?w) …)     ; optional
//!   (limit N))                    ; optional
//! ```
//!
//! The compiler is deliberately self-contained (no eval/channel concerns), so a
//! later slice can lift it verbatim into a language-agnostic transreptor. An
//! unknown head or malformed term is a clear, catchable error — never a panic.
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
//! ## Cacheability (opt-in, NetKernel-style)
//!
//! An eval is **uncacheable by default** — the safe posture, since a program may
//! `sink`/mutate. A program opts THIS eval in with `(cacheable expr)` (permanently
//! cacheable) or `(cacheable/ttl secs expr)` (cacheable for a max-age); both
//! evaluate `expr` and return its value, marking the eval as their side effect.
//! The opt-in is honoured only when it is *safe*:
//!
//! - **Mutation wins.** If any `Sink` or `Delete` verb was issued during the run,
//!   the result is forced uncacheable regardless of the opt-in — you cannot cache a
//!   side effect. (The verb contract decides this, not the sink's returned expiry.)
//! - **Never fresher than its inputs.** The kernel already folds every
//!   sub-request's expiry (`most_restrictive`) and golden threads onto the result
//!   after `invoke` returns, so an opted-in `(cacheable (source volatile))` inherits
//!   the source's volatility, and cutting a source's thread invalidates the cached
//!   eval — for free. The module sets only the *author's* ceiling.
//! - **`cacheable/ttl` needs a clock.** The max-age becomes an absolute
//!   `Expiry::At(now + secs)` via the kernel's injected clock; a clockless kernel
//!   declines to cache it (falls back to `Always`), mirroring ikigai-core.
//!
//! The opt-in reaches the async `invoke` side as a `CacheHint` control message on
//! the same channel the verb builtins use (see [`WorkerMsg`]); a program with no
//! `(cacheable …)` form leaves the default `Expiry::Always` untouched.
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
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Expiry, Invocation, Iri,
    ReprType, Representation, Request, Result, Verb,
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
(define (cacheable x) (%cache-permanent) x)
(define (cacheable/ttl secs x) (%cache-ttl secs) x)
(define (sparql-select query) (%sparql-select query))
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
        let (call_tx, call_rx) = crossbeam_channel::unbounded::<WorkerMsg>();
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

        // The two cacheability inputs, both observed here on the async side: whether
        // any mutating verb ran, and the program's opt-in ceiling (if any). See
        // [`decide_expiry`].
        let mut mutated = false;
        let mut author_expiry: Option<Expiry> = None;

        if sent.is_ok() {
            // `recv` returns `Err` once the worker finishes the run and drops the
            // eval's `call_tx` — the loop's natural exit. Both message kinds arrive
            // on this one channel, in program order.
            while let Ok(msg) = call_rx.recv() {
                match msg {
                    WorkerMsg::Verb(call) => {
                        // A Sink/Delete makes the eval a side effect — track it here
                        // from the VERB (before dispatch, so even a *denied* mutation
                        // still forbids caching), not from the sink's returned expiry.
                        if call.verb.is_mutating() {
                            mutated = true;
                        }
                        let result = dispatch(inv, &call).await;
                        // The evaluator parked on this reply; a send failure only means
                        // it went away first, which the next `recv` will observe.
                        let _ = call.reply.send(result);
                    }
                    WorkerMsg::Cache(hint) => {
                        // An opt-in `(cacheable …)` / `(cacheable/ttl …)` ran. Resolve
                        // its expiry (a ttl needs the kernel's clock) and fold it into
                        // any earlier opt-in, most-restrictive.
                        let e = hint.resolve(inv);
                        author_expiry =
                            Some(author_expiry.map_or(e, |prev| prev.most_restrictive(e)));
                    }
                }
            }
        }

        // A live worker sends exactly one result, then is safe to reuse. A worker
        // that died mid-run (a Steel panic) drops `result_tx`, so `recv` errors —
        // surface it as an endpoint error and let that worker go (do not re-pool).
        match result_rx.recv() {
            Ok(text) => {
                check_in_worker(worker);
                // Opt-in cacheability, honoured only when safe (see [`decide_expiry`]).
                // The kernel then folds every sub-request's expiry and golden threads
                // onto this result, so it is never fresher than its inputs — for free.
                let expiry = decide_expiry(mutated, author_expiry);
                Ok(Representation::new(text_plain_utf8(), text?.into_bytes()).with_expiry(expiry))
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
                 the result is the last form's value as text. Uncacheable by default; a program \
                 opts in with `(cacheable expr)` / `(cacheable/ttl secs expr)`, honoured only when \
                 no verb mutated and never fresher than the eval's own inputs.",
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

/// A message from the synchronous Steel side to the async servicing loop, over the
/// eval's one channel. A [`Verb`](WorkerMsg::Verb) is a kernel sub-request the
/// builtin parks on; a [`Cache`](WorkerMsg::Cache) is a fire-and-forget opt-in
/// signal from a `(cacheable …)` form (no reply). One channel keeps every
/// cacheability decision input on the `invoke` side, observed in program order.
enum WorkerMsg {
    /// A kernel verb sub-request (the builtin awaits the reply).
    Verb(VerbCall),
    /// The program opted this eval into caching (no reply expected).
    Cache(CacheHint),
}

/// A program's caching opt-in, raised by a `(cacheable …)` builtin. Resolved to an
/// [`Expiry`] on the async side, where the kernel's clock is reachable.
enum CacheHint {
    /// `(cacheable expr)` — permanently cacheable ([`Expiry::Never`]).
    Permanent,
    /// `(cacheable/ttl secs expr)` — cacheable for `secs` seconds. Becomes an
    /// absolute [`Expiry::At`] via the kernel's clock.
    Ttl(u64),
}

impl CacheHint {
    /// The author's chosen expiry ceiling. A `Ttl` needs the kernel's injected
    /// clock to turn a max-age into an absolute deadline; a clockless kernel cannot
    /// evaluate a deadline, so it declines to cache (`Always`) rather than risk
    /// serving forever — mirroring ikigai-core's clockless `At` behaviour.
    fn resolve(&self, inv: &Invocation<'_>) -> Expiry {
        match self {
            CacheHint::Permanent => Expiry::Never,
            CacheHint::Ttl(secs) => match inv.now() {
                Some(now) => Expiry::At(now.plus_millis(secs.saturating_mul(1000))),
                None => Expiry::Always,
            },
        }
    }
}

/// The eval result's own expiry ceiling, from the two safety inputs. A mutating
/// verb (Sink/Delete) forces `Always` regardless of any opt-in — you cannot cache a
/// side effect. Otherwise the program's opt-in ceiling applies, defaulting to
/// `Always` (uncacheable) when it never opted in. The kernel then meets this with
/// the eval's dependency expiries, so the result is never fresher than its inputs.
fn decide_expiry(mutated: bool, author_expiry: Option<Expiry>) -> Expiry {
    if mutated {
        Expiry::Always
    } else {
        author_expiry.unwrap_or(Expiry::Always)
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
    /// A `query=` argument (the compiled SPARQL of a `(sparql-select …)`). Distinct
    /// from `input`/`content` because `urn:sparql:select` reads its query from a
    /// named `query` arg, not the conventional `in`/`content`.
    query: Option<String>,
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
    if let Some(query) = &call.query {
        request = request.with_arg("query", ArgRef::Inline(query.clone().into_bytes()));
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
    call_tx: Sender<WorkerMsg>,
    result_tx: Sender<Result<String>>,
}

thread_local! {
    /// The servicing channel of the eval currently running on THIS worker thread.
    /// The worker sets it before each run and takes (drops) it after; the verb
    /// builtins read it at call time. A worker runs one eval at a time, so this is
    /// unambiguous — and it is what lets the builtins be registered once on a
    /// shared warm template yet route to the right (per-eval) servicing loop.
    static CURRENT_TX: RefCell<Option<Sender<WorkerMsg>>> = const { RefCell::new(None) };
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
    // The homoiconic SPARQL builtin: compile the quoted s-expr to a SPARQL SELECT
    // (pure, kernel-free) and issue it as a Source `query=` to `urn:sparql:select`.
    // A compile error becomes a catchable Steel error, exactly like a denied verb.
    engine.register_fn(
        "%sparql-select",
        |query: SteelVal| -> std::result::Result<String, String> {
            let sparql = sexpr_to_sparql(&query).map_err(|e| format!("{e}"))?;
            call_query("urn:sparql:select", sparql)
        },
    );
    // The opt-in signals: fire-and-forget cache hints (no reply). The Scheme
    // `(cacheable …)` wrappers call these, then return the evaluated value.
    engine.register_fn("%cache-permanent", || cache_hint(CacheHint::Permanent));
    engine.register_fn("%cache-ttl", |secs: isize| {
        // A negative max-age is meaningless; clamp to 0 (immediately stale) rather
        // than wrapping to a huge deadline.
        cache_hint(CacheHint::Ttl(secs.max(0) as u64))
    });
}

/// Raise a caching opt-in for the current eval: send a [`CacheHint`] over its
/// servicing channel. Fire-and-forget — the servicing loop folds it into the
/// result's expiry; there is no reply to park on. A missing/closed channel is
/// ignored (the loop has already decided). Returns `true` so the Scheme wrapper has
/// a value to discard.
fn cache_hint(hint: CacheHint) -> bool {
    if let Some(tx) = CURRENT_TX.with(|slot| slot.borrow().clone()) {
        let _ = tx.send(WorkerMsg::Cache(hint));
    }
    true
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
    tx.send(WorkerMsg::Verb(VerbCall {
        verb,
        iri,
        input,
        content,
        as_type,
        query: None,
        reply,
    }))
    .map_err(|_| "lisp: kernel channel closed".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "lisp: kernel dropped the reply".to_string())?
}

/// Send a Source sub-request carrying a `query=` argument (the compiled SPARQL of a
/// `(sparql-select …)`) to the current eval's servicing loop, blocking on the reply.
/// A thin sibling of [`call`] that routes through the same [`CURRENT_TX`] channel, so
/// the query is issued under this eval's capability and composes like any verb.
fn call_query(iri: &str, query: String) -> std::result::Result<String, String> {
    let tx = CURRENT_TX
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| "lisp: no active eval context".to_string())?;
    let (reply, reply_rx) = crossbeam_channel::bounded(1);
    tx.send(WorkerMsg::Verb(VerbCall {
        verb: Verb::Source,
        iri: iri.to_string(),
        input: None,
        content: None,
        as_type: None,
        query: Some(query),
        reply,
    }))
    .map_err(|_| "lisp: kernel channel closed".to_string())?;
    reply_rx
        .recv()
        .map_err(|_| "lisp: kernel dropped the reply".to_string())?
}

// =====================================================================================
// The homoiconic SPARQL compiler — a pure `SteelVal` → `String`, kernel-free.
//
// This is the reusable core of `(sparql-select …)`. It is deliberately free of any
// eval, channel, capability or kernel concern, so a later slice can lift it verbatim
// into a language-agnostic transreptor. Everything below is a total function of its
// s-expression input: string literals and IRIs are escaped/validated (nothing is
// interpolated raw), and any malformed input is a clear [`Error`] — never a panic.
// =====================================================================================

/// Compile a quoted s-expression describing a SELECT into a SPARQL query string.
///
/// The accepted grammar is documented on the [crate] docs. The head symbol must be
/// `select`; an unknown head, a malformed clause, or an unsupported term yields an
/// `Err` the `(sparql-select …)` builtin surfaces as a catchable Steel error.
pub fn sexpr_to_sparql(value: &SteelVal) -> Result<String> {
    let items = list_items(value).ok_or_else(|| {
        compile_err("query must be a list, e.g. (select (?s ?p ?o) (where (?s ?p ?o)))")
    })?;
    let head = items
        .first()
        .and_then(|v| symbol_name(v))
        .ok_or_else(|| compile_err("query must start with a head symbol (e.g. `select`)"))?;
    match head {
        "select" => compile_select(&items[1..]),
        other => Err(compile_err(&format!(
            "unknown query head `{other}`; only `select` is supported"
        ))),
    }
}

/// Compile the tail of a `(select …)` form: `PROJECTION CLAUSE…`, where PROJECTION is a
/// var list or `*` and each CLAUSE is `where`/`prefix`/`limit`/`order-by`. Clauses may
/// appear in any order; the emitted query always orders them canonically
/// (PREFIX → SELECT/WHERE → ORDER BY → LIMIT).
fn compile_select(rest: &[&SteelVal]) -> Result<String> {
    let projection = rest.first().ok_or_else(|| {
        compile_err("select needs a projection: (select (?a …) …) or (select * …)")
    })?;
    let proj = compile_projection(projection)?;

    let mut prefixes: Vec<String> = Vec::new();
    let mut where_triples: Vec<String> = Vec::new();
    let mut limit: Option<String> = None;
    let mut order_by: Option<String> = None;
    let mut saw_where = false;

    for clause in &rest[1..] {
        let citems = list_items(clause)
            .ok_or_else(|| compile_err("each select clause must be a list, e.g. (where …)"))?;
        let chead = citems.first().and_then(|v| symbol_name(v)).ok_or_else(|| {
            compile_err("a select clause must start with a keyword (where/prefix/limit/order-by)")
        })?;
        match chead {
            "where" => {
                saw_where = true;
                for triple in &citems[1..] {
                    where_triples.push(compile_triple(triple)?);
                }
            }
            "prefix" => {
                for binding in &citems[1..] {
                    prefixes.push(compile_prefix(binding)?);
                }
            }
            "limit" => limit = Some(compile_limit(&citems[1..])?),
            "order-by" => order_by = Some(compile_order_by(&citems[1..])?),
            other => {
                return Err(compile_err(&format!(
                    "unknown select clause `{other}`; expected where/prefix/limit/order-by"
                )))
            }
        }
    }
    if !saw_where {
        return Err(compile_err("select needs a (where …) clause"));
    }

    let mut out = String::new();
    for line in &prefixes {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("SELECT ");
    out.push_str(&proj);
    out.push_str(" WHERE {");
    for triple in &where_triples {
        out.push_str("\n  ");
        out.push_str(triple);
    }
    out.push_str("\n}");
    if let Some(order) = &order_by {
        out.push('\n');
        out.push_str(order);
    }
    if let Some(lim) = &limit {
        out.push('\n');
        out.push_str(lim);
    }
    Ok(out)
}

/// The projection: `*` (a lone symbol) or a non-empty list of `?variables`.
fn compile_projection(value: &SteelVal) -> Result<String> {
    if symbol_name(value) == Some("*") {
        return Ok("*".to_string());
    }
    let vars = list_items(value)
        .ok_or_else(|| compile_err("projection must be a var list (?a ?b …) or *"))?;
    if vars.is_empty() {
        return Err(compile_err(
            "projection var list is empty; use * to select all",
        ));
    }
    let mut rendered = Vec::with_capacity(vars.len());
    for v in vars {
        rendered.push(render_var(v)?);
    }
    Ok(rendered.join(" "))
}

/// A single triple pattern `(S P O)` → `S P O .`. The predicate additionally accepts
/// the bare symbol `a` (SPARQL's `rdf:type` keyword).
fn compile_triple(value: &SteelVal) -> Result<String> {
    let terms = list_items(value).ok_or_else(|| compile_err("a triple must be a list (S P O)"))?;
    if terms.len() != 3 {
        return Err(compile_err(
            "a triple must have exactly three terms (S P O)",
        ));
    }
    let s = render_term(terms[0])?;
    let p = render_predicate(terms[1])?;
    let o = render_term(terms[2])?;
    Ok(format!("{s} {p} {o} ."))
}

/// A `(prefix (pfx "http://…") …)` binding → a `PREFIX pfx: <http://…>` line. The name
/// may be written `pfx` or `pfx:` (the colon is added on emit).
fn compile_prefix(value: &SteelVal) -> Result<String> {
    let items = list_items(value)
        .ok_or_else(|| compile_err("a prefix binding must be a list (pfx \"http://…\")"))?;
    if items.len() != 2 {
        return Err(compile_err("a prefix binding is (pfx \"http://…\")"));
    }
    let raw = symbol_name(items[0])
        .ok_or_else(|| compile_err("prefix name must be a symbol, e.g. rdf"))?;
    let pfx = raw.strip_suffix(':').unwrap_or(raw);
    if !valid_pn_prefix(pfx) {
        return Err(compile_err(&format!("invalid prefix name `{pfx}`")));
    }
    let iri = match items[1] {
        SteelVal::StringV(s) => render_iri(s.as_str())?,
        _ => return Err(compile_err("a prefix namespace must be a string IRI")),
    };
    Ok(format!("PREFIX {pfx}: {iri}"))
}

/// A `(limit N)` modifier — one non-negative integer.
fn compile_limit(args: &[&SteelVal]) -> Result<String> {
    if args.len() == 1 {
        match args[0] {
            SteelVal::IntV(n) if *n >= 0 => return Ok(format!("LIMIT {n}")),
            SteelVal::IntV(_) => return Err(compile_err("limit must be a non-negative integer")),
            _ => {}
        }
    }
    Err(compile_err(
        "limit takes a single non-negative integer, e.g. (limit 10)",
    ))
}

/// An `(order-by ?v (desc ?w) …)` modifier — one or more conditions.
fn compile_order_by(args: &[&SteelVal]) -> Result<String> {
    if args.is_empty() {
        return Err(compile_err("order-by needs at least one ?variable"));
    }
    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        parts.push(render_order_condition(arg)?);
    }
    Ok(format!("ORDER BY {}", parts.join(" ")))
}

/// One ORDER BY condition: a bare `?variable`, or `(asc ?v)` / `(desc ?v)`.
fn render_order_condition(value: &SteelVal) -> Result<String> {
    if let Some(s) = symbol_name(value) {
        return if is_var(s) {
            Ok(s.to_string())
        } else {
            Err(compile_err("order-by symbol must be a ?variable"))
        };
    }
    let items = list_items(value)
        .ok_or_else(|| compile_err("order-by term must be ?var or (asc ?var)/(desc ?var)"))?;
    let keyword = match items.first().and_then(|v| symbol_name(v)) {
        Some("asc") => "ASC",
        Some("desc") => "DESC",
        _ => return Err(compile_err("order-by direction must be asc or desc")),
    };
    if items.len() != 2 {
        return Err(compile_err("(asc …)/(desc …) takes one ?variable"));
    }
    let var = render_var(items[1])?;
    Ok(format!("{keyword}({var})"))
}

/// A term in subject/object position: `?var` | `(iri "…")` | `pfx:local` | string | number.
fn render_term(value: &SteelVal) -> Result<String> {
    match value {
        SteelVal::SymbolV(s) => render_symbol_term(s.as_str()),
        SteelVal::StringV(s) => Ok(render_string_literal(s.as_str())),
        SteelVal::IntV(n) => Ok(n.to_string()),
        SteelVal::NumV(n) => {
            if n.is_finite() {
                Ok(format!("{n}"))
            } else {
                Err(compile_err(
                    "a non-finite number is not a valid SPARQL literal",
                ))
            }
        }
        SteelVal::ListV(list) => {
            let items: Vec<&SteelVal> = list.iter().collect();
            match items.first().and_then(|v| symbol_name(v)) {
                Some("iri") => render_iri_form(&items[1..]),
                Some(other) => Err(compile_err(&format!(
                    "unknown term form `({other} …)`; only (iri \"…\") is a compound term"
                ))),
                None => Err(compile_err(
                    "a compound term must start with a symbol, e.g. (iri \"…\")",
                )),
            }
        }
        _ => Err(compile_err(
            "unsupported term; use ?var, (iri \"…\"), pfx:local, a string, or a number",
        )),
    }
}

/// A predicate term: like [`render_term`], but also accepting the bare `a` keyword.
fn render_predicate(value: &SteelVal) -> Result<String> {
    if symbol_name(value) == Some("a") {
        return Ok("a".to_string());
    }
    render_term(value)
}

/// A symbol used as a term: either a `?variable` or a `pfx:local` prefixed name.
fn render_symbol_term(s: &str) -> Result<String> {
    if is_var(s) || is_prefixed_name(s) {
        Ok(s.to_string())
    } else {
        Err(compile_err(&format!(
            "unrecognized term symbol `{s}`; use ?var or pfx:local (full IRIs go in (iri \"…\"))"
        )))
    }
}

/// A `?variable` term, validated (only `?` + word characters) to keep it injection-safe.
fn render_var(value: &SteelVal) -> Result<String> {
    match symbol_name(value) {
        Some(s) if is_var(s) => Ok(s.to_string()),
        _ => Err(compile_err("expected a ?variable")),
    }
}

/// The single-argument body of an `(iri "…")` compound term → a validated `<…>` IRIREF.
fn render_iri_form(args: &[&SteelVal]) -> Result<String> {
    if args.len() == 1 {
        if let SteelVal::StringV(s) = args[0] {
            return render_iri(s.as_str());
        }
    }
    Err(compile_err(
        "(iri …) takes exactly one string, e.g. (iri \"http://…\")",
    ))
}

/// Wrap a validated IRI in `<…>`. Rejects any character illegal in a SPARQL IRIREF
/// (angle brackets, quotes, braces, backslash, control chars, whitespace) — so a term
/// can never break out of the IRIREF and inject query syntax.
fn render_iri(iri: &str) -> Result<String> {
    if iri.is_empty() {
        return Err(compile_err("(iri \"\") is empty"));
    }
    if iri.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\' | ' '
            )
    }) {
        return Err(compile_err(&format!(
            "IRI `{iri}` contains characters not allowed in a SPARQL IRIREF"
        )));
    }
    Ok(format!("<{iri}>"))
}

/// Render a Rust string as a SPARQL string literal, escaping the reserved characters
/// so the literal cannot terminate early or inject syntax.
fn render_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `SteelVal`s of a list, or `None` if it is not a list.
fn list_items(value: &SteelVal) -> Option<Vec<&SteelVal>> {
    match value {
        SteelVal::ListV(list) => Some(list.iter().collect()),
        _ => None,
    }
}

/// The name of a symbol value, or `None` if it is not a symbol.
fn symbol_name(value: &SteelVal) -> Option<&str> {
    match value {
        SteelVal::SymbolV(s) => Some(s.as_str()),
        _ => None,
    }
}

/// A SPARQL variable: `?` followed by one or more ASCII word characters.
fn is_var(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('?') {
        return false;
    }
    let rest = chars.as_str();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A conservative prefixed name `pfx:local` — a valid prefix and a word-ish local part.
fn is_prefixed_name(s: &str) -> bool {
    match s.find(':') {
        Some(idx) => valid_pn_prefix(&s[..idx]) && valid_pn_local(&s[idx + 1..]),
        None => false,
    }
}

/// A conservative SPARQL prefix label: empty (the default prefix) or a letter then
/// letters/digits.
fn valid_pn_prefix(pfx: &str) -> bool {
    if pfx.is_empty() {
        return true;
    }
    let mut chars = pfx.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// A conservative SPARQL local name: word characters and hyphens (may be empty).
fn valid_pn_local(local: &str) -> bool {
    local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A compile-time error from [`sexpr_to_sparql`], surfaced to the program as a catchable
/// Steel error (identical treatment to a denied verb) — never a panic.
fn compile_err(msg: &str) -> Error {
    Error::Endpoint(format!("sparql-select: {msg}"))
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
        // A permanently-cacheable source (`Expiry::Never`) and a volatile one
        // (`Expiry::Always`, the default) — so a `(cacheable (source …))` can be
        // shown to inherit its input's volatility.
        let pure = FnEndpoint::new("pure", |_inv: &Invocation<'_>| {
            Ok(Representation::new(text_plain_utf8(), b"pure".to_vec()).cacheable())
        });
        let volatile = FnEndpoint::new("volatile", |_inv: &Invocation<'_>| {
            Ok(Representation::new(text_plain_utf8(), b"live".to_vec()))
        });
        // A cacheable source that declares a golden thread for its mutable state —
        // to prove those threads flow onto a cached eval automatically (the module
        // never sets threads; cutting this thread invalidates the eval that read it).
        let threaded = FnEndpoint::new("threaded", |_inv: &Invocation<'_>| {
            Ok(Representation::new(text_plain_utf8(), b"v1".to_vec())
                .cacheable()
                .depends_on("urn:test:threaded"))
        });
        let space = ikigai_fn::space()
            .bind(Exact::new("urn:lisp:eval"), eval())
            .bind(Exact::new("urn:test:vault"), DenyingVault)
            .bind(Exact::new("urn:test:ping"), ping)
            .bind(Exact::new("urn:test:pure"), pure)
            .bind(Exact::new("urn:test:volatile"), volatile)
            .bind(Exact::new("urn:test:threaded"), threaded);
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

    // ---- cacheability: opt-in, mutation-safe, never fresher than inputs ----

    /// A capability that also grants `urn:cap:test:write`, so the vault sink
    /// *succeeds* (exercising an ALLOWED mutation, not just a denied one).
    fn writing_cap() -> Capability {
        Capability::scoped(["urn:cap:lisp", "urn:cap:test:write"])
    }

    /// A fixed clock, so a `cacheable/ttl` deadline is deterministic.
    struct FixedClock(u64);
    impl ikigai_core::Clock for FixedClock {
        fn now(&self) -> ikigai_core::Time {
            ikigai_core::Time::from_millis(self.0)
        }
    }

    #[test]
    fn no_opt_in_stays_uncacheable() {
        // Rule 3 (default): without `(cacheable …)`, the result keeps the safe
        // `Always` default — a plain eval may mutate.
        let rep = try_eval(&lisp_cap(), "(+ 1 2)").unwrap();
        assert_eq!(rep.expiry, Expiry::Always);
    }

    #[test]
    fn opt_in_makes_a_pure_eval_permanently_cacheable() {
        // Rule: `(cacheable expr)` on a pure eval → `Never`.
        let rep = try_eval(&lisp_cap(), "(cacheable (+ 1 2))").unwrap();
        assert_eq!(rep.expiry, Expiry::Never);
        assert_eq!(String::from_utf8(rep.bytes).unwrap(), "3"); // still returns expr's value
    }

    #[test]
    fn a_cacheable_eval_is_served_from_the_cache_on_re_resolution() {
        // The opt-in genuinely participates in the kernel cache: a second identical
        // resolve of a `(cacheable …)` eval is a HIT (one kernel across both issues).
        let k = kernel();
        let cap = lisp_cap();
        let req = || {
            Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
                .with_arg("in", ArgRef::Inline(b"(cacheable (+ 1 2))".to_vec()))
        };
        assert!(!k.is_cached(&req(), &cap), "not cached before first issue");
        assert_eq!(
            String::from_utf8(block_on(k.issue(req(), &cap)).unwrap().bytes).unwrap(),
            "3"
        );
        assert!(
            k.is_cached(&req(), &cap),
            "a cacheable eval is cached after the first resolve"
        );
    }

    #[test]
    fn a_denied_mutation_forces_uncacheable_despite_opt_in() {
        // Rule (mutation wins): a Sink was issued — even though DENIED and caught —
        // so the opt-in is ignored and the result stays `Always`.
        let rep = try_eval(
            &lisp_cap(),
            r#"(cacheable (with-handler (lambda (e) "caught") (sink "urn:test:vault" "x")))"#,
        )
        .unwrap();
        assert_eq!(String::from_utf8(rep.bytes.clone()).unwrap(), "caught");
        assert_eq!(rep.expiry, Expiry::Always, "a mutation forbids caching");
    }

    #[test]
    fn an_allowed_mutation_forces_uncacheable_despite_opt_in() {
        // Rule (mutation wins), the stronger case: the sink SUCCEEDS (write grant),
        // a real side effect — the opt-in is still ignored, result `Always`.
        let rep = try_eval(&writing_cap(), r#"(cacheable (sink "urn:test:vault" "x"))"#).unwrap();
        assert_eq!(String::from_utf8(rep.bytes.clone()).unwrap(), "stored");
        assert_eq!(
            rep.expiry,
            Expiry::Always,
            "a successful write forbids caching"
        );
    }

    #[test]
    fn opt_in_over_a_pure_source_stays_never() {
        // A cacheable source under the opt-in → still `Never` (both permanent).
        let rep = try_eval(&lisp_cap(), r#"(cacheable (source "urn:test:pure"))"#).unwrap();
        assert_eq!(rep.expiry, Expiry::Never);
    }

    #[test]
    fn opt_in_over_a_volatile_source_inherits_its_volatility() {
        // Rule (never fresher than inputs): the SAME opt-in over a volatile source
        // (`Always`) is clamped by the kernel's dependency fold to `Always`, NOT the
        // `Never` the author asked for. Contrast with `opt_in_over_a_pure_source…`.
        let rep = try_eval(&lisp_cap(), r#"(cacheable (source "urn:test:volatile"))"#).unwrap();
        assert_eq!(
            rep.expiry,
            Expiry::Always,
            "a cacheable eval is no fresher than its most volatile input"
        );
    }

    #[test]
    fn ttl_opt_in_with_a_clock_yields_a_deadline() {
        // Rule: `(cacheable/ttl secs expr)` under a kernel WITH a clock → `At(now + secs)`.
        let space = ikigai_fn::space().bind(Exact::new("urn:lisp:eval"), eval());
        let k = Kernel::new(Arc::new(space)).with_clock(Arc::new(FixedClock(1_000)));
        let req = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap()).with_arg(
            "in",
            ArgRef::Inline(b"(cacheable/ttl 300 (+ 1 2))".to_vec()),
        );
        let rep = block_on(k.issue(req, &lisp_cap())).unwrap();
        // now = 1000 ms, +300 s = +300_000 ms → deadline 301_000.
        assert_eq!(
            rep.expiry,
            Expiry::At(ikigai_core::Time::from_millis(301_000))
        );
    }

    #[test]
    fn ttl_opt_in_without_a_clock_declines_to_cache() {
        // Rule: a clockless kernel cannot evaluate a deadline, so a `cacheable/ttl`
        // opt-in falls back to `Always` rather than risk serving forever.
        let rep = try_eval(&lisp_cap(), "(cacheable/ttl 300 (+ 1 2))").unwrap();
        assert_eq!(rep.expiry, Expiry::Always);
    }

    #[test]
    fn cutting_a_sourced_thread_invalidates_the_cached_eval() {
        // Threads stay AUTOMATIC: the module never sets threads, yet the golden
        // thread declared by a sourced resource flows onto the cached eval via the
        // kernel's dependency union — so cutting it invalidates the eval that read it.
        let k = kernel();
        let cap = lisp_cap();
        let req = || {
            Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap()).with_arg(
                "in",
                ArgRef::Inline(b"(cacheable (source \"urn:test:threaded\"))".to_vec()),
            )
        };
        block_on(k.issue(req(), &cap)).unwrap();
        assert!(k.is_cached(&req(), &cap), "cached after first resolve");
        k.cut("urn:test:threaded");
        assert!(
            !k.is_cached(&req(), &cap),
            "cutting the sourced resource's thread invalidated the cached eval"
        );
    }

    // ---- the homoiconic SPARQL compiler (pure, kernel-free) ----------------

    /// Read a Steel source form to the `SteelVal` the `(sparql-select …)` builtin
    /// receives — i.e. the exact quoted-list value the real reader produces. Uses a
    /// bare Steel engine, NOT the kernel: the compiler is proven kernel-free here.
    fn parse(src: &str) -> SteelVal {
        Engine::new()
            .run(src.to_string())
            .expect("test s-expr must read")
            .last()
            .expect("a value")
            .clone()
    }

    #[test]
    fn compiles_a_basic_select_with_one_triple() {
        // The base case: a SELECT of three vars over a single triple pattern.
        let q = parse("'(select (?s ?p ?o) (where (?s ?p ?o)))");
        assert_eq!(
            sexpr_to_sparql(&q).unwrap(),
            "SELECT ?s ?p ?o WHERE {\n  ?s ?p ?o .\n}"
        );
    }

    #[test]
    fn compiles_prefix_projection_and_limit() {
        // PREFIX line, a narrowed projection, a prefixed-name predicate, and LIMIT —
        // emitted in canonical order regardless of clause order in the s-expr.
        let q = parse(
            r#"'(select (?s)
                 (limit 5)
                 (prefix (rdf "http://www.w3.org/1999/02/22-rdf-syntax-ns#"))
                 (where (?s rdf:type ?o)))"#,
        );
        assert_eq!(
            sexpr_to_sparql(&q).unwrap(),
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n\
             SELECT ?s WHERE {\n  ?s rdf:type ?o .\n}\nLIMIT 5"
        );
    }

    #[test]
    fn compiles_star_projection_iri_term_and_order_by() {
        // `*` projection, an (iri "…") subject, the `a` predicate keyword, and an
        // ORDER BY mixing a bare var with a (desc …) condition.
        let q = parse(
            r#"'(select *
                 (where ((iri "http://example.org/s") a ?o))
                 (order-by ?o (desc ?o)))"#,
        );
        assert_eq!(
            sexpr_to_sparql(&q).unwrap(),
            "SELECT * WHERE {\n  <http://example.org/s> a ?o .\n}\nORDER BY ?o DESC(?o)"
        );
    }

    #[test]
    fn string_literals_and_iris_are_escaped_not_interpolated() {
        // Safety is the point vs string-building: a literal's quotes/backslashes are
        // escaped, and an IRI carrying query syntax is rejected outright (never emitted).
        assert_eq!(render_string_literal("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert!(
            render_iri("http://ex/x> . } INJECT {").is_err(),
            "an IRI with IRIREF-illegal characters must be rejected"
        );
    }

    #[test]
    fn an_unknown_query_head_is_an_error_not_a_panic() {
        // The required negative: an unknown head is a catchable `Err`, no panic.
        assert!(sexpr_to_sparql(&parse("'(delete-everything)")).is_err());
    }

    #[test]
    fn malformed_queries_are_rejected() {
        // A non-list, an unknown clause, and a bad triple arity are all clean errors.
        assert!(sexpr_to_sparql(&parse("42")).is_err());
        assert!(sexpr_to_sparql(&parse("'(select (?s) (drop-table (?s ?p ?o)))")).is_err());
        assert!(sexpr_to_sparql(&parse("'(select (?s) (where (?s ?p)))")).is_err());
        assert!(sexpr_to_sparql(&parse("'(select (?s))")).is_err()); // no where clause
    }

    // ---- end-to-end: `(sparql-select …)` through a real SPARQL engine -------

    /// A kernel that binds the real `ikigai-sparql` module (all four query verbs) plus
    /// the Lisp eval endpoint — so `(sparql-select …)` runs a compiled query end-to-end.
    fn sparql_kernel() -> Kernel {
        let space = ikigai_sparql::space().bind(Exact::new("urn:lisp:eval"), eval());
        Kernel::new(Arc::new(space))
    }

    /// Evaluate `src` against [`sparql_kernel`] and return the body text.
    fn sparql_eval_ok(src: &str) -> String {
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()));
        String::from_utf8(
            block_on(sparql_kernel().issue(request, &lisp_cap()))
                .unwrap()
                .bytes,
        )
        .unwrap()
    }

    #[test]
    fn sparql_select_runs_a_compiled_query_end_to_end() {
        // The always-loaded ikigai vocabulary guarantees a non-empty solution set, so
        // `?s ?p ?o` with LIMIT 1 returns exactly one binding as sparql-results JSON.
        let out =
            sparql_eval_ok(r#"(sparql-select '(select (?s ?p ?o) (where (?s ?p ?o)) (limit 1)))"#);
        assert!(
            out.contains("\"bindings\""),
            "expected sparql-results JSON, got: {out}"
        );
        assert!(
            out.contains("\"value\""),
            "expected a non-empty solution set, got: {out}"
        );
    }

    #[test]
    fn a_quasiquote_composed_query_matches_the_literal() {
        // Homoiconic composition: splicing a var list (`,vars`) and patterns (`,@…`)
        // into a quasiquoted query yields the SAME result as writing it literally —
        // the query is built by composition, never string concatenation.
        let literal =
            sparql_eval_ok(r#"(sparql-select '(select (?s ?p ?o) (where (?s ?p ?o)) (limit 1)))"#);
        let composed = sparql_eval_ok(
            r#"(define vars '(?s ?p ?o))
               (define patterns (list '(?s ?p ?o)))
               (sparql-select `(select ,vars (where ,@patterns) (limit 1)))"#,
        );
        assert_eq!(composed, literal);
    }

    #[test]
    fn a_bad_query_sexpr_is_catchable_in_lisp() {
        // A compile error is a catchable Steel error (like a denied verb), trapped by
        // `with-handler` — the program recovers, no panic, no query issued.
        assert_eq!(
            sparql_eval_ok(r#"(with-handler (lambda (e) "caught") (sparql-select '(nonsense)))"#,),
            "caught"
        );
    }
}
