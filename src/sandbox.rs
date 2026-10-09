//! The sandbox: the program-visible world, built up from what is NEEDED.
//!
//! A program holding only `urn:cap:lisp` reaches the world through the kernel verbs and
//! nothing else (audit round 6, ledger #903: H-F2, C-R1–R4). Steel's `new_sandboxed`
//! engine blocks dylib loading and the direct file builtins, and leaves some 1,800 globals
//! bound beside them — environment and argv readers, process and git modules, runtime
//! `eval`, an engine constructor, the host's stdio ports — so this module does not try to
//! name what is dangerous. It names what is needed, and refuses everything else:
//!
//! - **Names.** After the engine is built and the prelude has run, every global that is
//!   not on the allowlist is rebound to a refusal ([`lock_down`]). A Steel upgrade that
//!   adds a global adds a refused one. The allowlist is three lists: this crate's own
//!   surface ([`PRELUDE_NAMES`]), the Steel names programs use ([`PROGRAM_NAMES`]), and
//!   what those need internally ([`STDLIB_INTERNALS`]) — the last DERIVED, by closing
//!   the second over the reference graph of Steel's compiled stdlib, never by trying
//!   things against the host. `docs/design/sandbox-allowlist.md` says how.
//! - **Macros.** Only [`MACROS`] stay in scope.
//! - **Forms that leave the evaluation.** `require` of any path, `require-builtin`,
//!   `defmacro`, `begin-for-syntax` and a `define-syntax` that is not `syntax-rules` are
//!   refused before compilation ([`screen`]): the last three run program code at
//!   EXPANSION time, in the compiler's own macro engine, which this crate cannot lock
//!   down. A catch-all module resolver refuses any path `require` that got past it.
//!
//! The pins are in this file's tests: the names a program can reach after build EQUAL the
//! allowlist, and the derived list equals the closure of the program list.

use std::collections::HashSet;
use std::sync::Arc;

use steel::compiler::modules::SourceModuleResolver;
use steel::parser::ast::ExprKind;
use steel::parser::parser::{lower_macro_and_require_definitions, Parser};
use steel::parser::tokens::TokenType;
use steel::rerrs::{ErrorKind, SteelErr};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;

/// A global on the allowlist: a top-level name, or a definition inside one of Steel's
/// stdlib modules, named by module path because the mangled prefix the compiler gives it
/// depends on what was interned first.
pub(crate) enum Name {
    Top(&'static str),
    In(&'static str, &'static str),
}

use Name::{In, Top};

/// This crate's own surface: the verbs and the `read` the prelude defines, and the
/// primitives and reader functions they call. Every name here is defined by
/// `PRELUDE`/`PRELUDE_READ`/`register_primitives` in `lib.rs`.
#[rustfmt::skip] // grouped by purpose, a line per group
pub(crate) const PRELUDE_NAMES: &[&str] = &[
    // The verbs and their friends (`PRELUDE`).
    "source", "sink", "meta", "exists", "delete", "invoke", "sparql-select", "graph",
    "input", "cacheable", "cacheable/ttl",
    // The primitives under them (`register_primitives`). `%verb-args` is also called
    // directly by the alias prelude a host generates (`urn:lisp:aliases`).
    "%source", "%source-in", "%sink", "%meta", "%meta-as", "%exists", "%delete", "%input",
    "%sparql-select", "%verb-args", "%graph", "%cache-permanent", "%cache-ttl",
    // `read`, kept per port (`PRELUDE_READ`), and the four reader primitives it uses.
    "read", "%read-port", "%read-state", "%read-one-datum", "%read-drain",
    "%read-tail-is-blank?", "%reader.new-reader", "%reader.reader-push-string",
    "%reader.reader-read-one", "%reader.#%intern",
];

/// What programs need from Steel: every Steel global the known programs reference after
/// expansion — the ikigai-programs corpus, this crate's tests and README, the runbook's
/// examples and the alias prelude a host generates (derivation in the design note).
#[rustfmt::skip] // grouped by purpose, a line per group
pub(crate) const PROGRAM_NAMES: &[&str] = &[
    // Numbers.
    "+", "-", "*", "<", "<=", "=", ">", ">=", "modulo", "quotient", "integer?", "number?",
    "number->string", "string->number",
    // Lists.
    "list", "cons", "car", "cdr", "cadr", "caddr", "cddr", "append", "reverse", "length",
    "list?", "null?", "map", "filter", "assoc", "vector",
    // Strings and characters.
    "string?", "string-append", "string-length", "string-upcase", "string-downcase",
    "substring", "string->list", "list->string", "split-whitespace", "ends-with?",
    "to-string", "symbol->string", "char->integer", "utf8-length",
    // Equality, truth, data in.
    "equal?", "not", "open-input-string", "eof-object?",
    // What the allowed macros expand into: `with-handler` (through `reset`/`shift`)
    // and the boxes Steel uses for internal definitions.
    "call-with-exception-handler", "*reset", "*shift", "#%box", "#%unbox", "#%set-box!",
];

/// What the stdlib needs internally to serve [`PROGRAM_NAMES`] and the prelude: the
/// closure of those names over the reference graph of Steel's compiled stdlib, minus the
/// names themselves. DERIVED — `tests::the_internals_are_the_closure_of_the_program_names`
/// recomputes it and fails when Steel's internals move.
#[rustfmt::skip] // grouped by purpose, a line per group
pub(crate) const STDLIB_INTERNALS: &[Name] = &[
    // Primitives the stdlib's own definitions call by their `#%prim.` names.
    Top("#%prim.="), Top("#%prim.apply"), Top("#%prim.call/cc"), Top("#%prim.car"),
    Top("#%prim.cdr"), Top("#%prim.cons"), Top("#%prim.equal?"), Top("#%prim.length"),
    Top("#%prim.null?"), Top("#%prim.pair?"), Top("#%prim.reverse"),
    Top("#%prim.#%read-port-to-string"),
    // Thread-local storage, which delimited continuations (`with-handler`) keep their
    // meta-continuation in.
    Top("#%prim.make-tls"), Top("#%prim.get-tls"), Top("#%prim.set-tls!"),
    // The continuation machinery behind `with-handler`.
    In("#%private/steel/stdlib", "*abort"),
    In("#%private/steel/stdlib", "*meta-continuation*"),
    In("#%private/steel/stdlib", "*reset"),
    In("#%private/steel/stdlib", "*shift"),
    // The stdlib definitions behind the list names programs use.
    In("#%private/steel/stdlib", "map"), In("#%private/steel/stdlib", "filter"),
    In("#%private/steel/stdlib", "assoc"), In("#%private/steel/stdlib", "cadr"),
    In("#%private/steel/stdlib", "caddr"), In("#%private/steel/stdlib", "cddr"),
    // What the prelude's `read` uses: drain a string port, trim it, say eof or error.
    Top("#%string-input-port?"), Top("read-port-to-string"),
    In("#%private/steel/ports", "read-port-to-string"), Top("trim"), Top("eof-object"),
    Top("void?"), Top("eq?"), Top("error"), Top("error!"),
];

/// The macros left in scope: the ones programs use, and the two `with-handler` expands
/// into. Every other macro is removed, so it reads as an ordinary (refused) call.
#[rustfmt::skip] // grouped by purpose, a line per group
pub(crate) const MACROS: &[&str] = &[
    "and", "or", "cond", "let*", "quasiquote", "with-handler", "reset", "shift",
];

/// Compiler-generated functions the stdlib's definitions are lifted into. Their names
/// carry a parse counter, so they cannot be listed; they are kept as a CLASS because a
/// lifted function is capture-free code (Steel lifts a lambda only when it captures
/// nothing), so it holds no authority of its own — it reaches only globals, which are
/// allowed or refused like any other. A test asserts every one is a capture-free closure.
pub(crate) const LIFTED_PREFIX: &str = "##__lifted_pure_function";

/// The phrase every sandbox refusal carries, so an uncaught one leaves the eval as a typed
/// `Denied` rather than an endpoint failure (see `lib.rs`).
pub(crate) const REFUSAL_MARK: &str = "is not available to a program";

/// The forms refused before compilation, wherever they appear.
const REFUSED_FORMS: &[&str] = &["require", "require-builtin", "defmacro", "begin-for-syntax"];

/// Every name on the allowlist, resolved to the global it names in `engine`.
pub(crate) fn allowed(engine: &Engine) -> HashSet<String> {
    let prefixes: Vec<(String, String)> = engine
        .modules()
        .iter()
        .map(|(path, module)| {
            (
                path.to_string_lossy().into_owned(),
                module.prefix().to_string(),
            )
        })
        .collect();
    let mut names: HashSet<String> = PRELUDE_NAMES
        .iter()
        .chain(PROGRAM_NAMES)
        .map(|n| n.to_string())
        .collect();
    for name in STDLIB_INTERNALS {
        match name {
            Top(n) => {
                names.insert(n.to_string());
            }
            In(path, n) => {
                if let Some((_, prefix)) = prefixes.iter().find(|(p, _)| p == path) {
                    names.insert(format!("{prefix}{n}"));
                }
            }
        }
    }
    names
}

/// The value a refused global is rebound to: a function that refuses, naming itself, so
/// calling it is a catchable error and passing it around reaches nothing.
fn refusal(name: &str) -> SteelVal {
    let message = format!(
        "`{name}` {REFUSAL_MARK} (ikigai-lisp's sandbox allows the kernel verbs and a \
         short list of pure functions; see src/sandbox.rs)"
    );
    SteelVal::anonymous_boxed_function(Arc::new(move |_args: &[SteelVal]| {
        Err(SteelErr::new(ErrorKind::Generic, message.clone()))
    }))
}

/// Refuse every global not on the allowlist, drop every macro not in [`MACROS`], and
/// refuse every path `require`. Run on each engine after its prelude, before any program.
pub(crate) fn lock_down(engine: &mut Engine) {
    let allowed = allowed(engine);
    let mut names: Vec<String> = engine
        .globals()
        .iter()
        .map(|n| n.resolve().to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        if allowed.contains(&name) || name.starts_with(LIFTED_PREFIX) {
            continue;
        }
        engine.update_value(&name, refusal(&name));
    }
    engine
        .in_scope_macros_mut()
        .retain(|name, _| MACROS.contains(&name.resolve()));
    engine.register_source_module_resolver(RefuseEveryPath);
}

/// A module resolver that claims every path and resolves none. Steel consults resolvers
/// BEFORE the filesystem, so a path `require` that got past [`screen`] fails to resolve
/// instead of reading a file.
struct RefuseEveryPath;

impl SourceModuleResolver for RefuseEveryPath {
    fn resolve(&self, _key: &str) -> Option<String> {
        None
    }

    fn exists(&self, _key: &str) -> bool {
        true
    }
}

/// Refuse a program that uses a form which leaves the evaluation, before it compiles:
/// `require`/`require-builtin` (modules, files, host builtins), and the three ways a
/// program runs its own code at EXPANSION time in the compiler's macro engine —
/// `defmacro`, `begin-for-syntax`, and a `define-syntax` that the compiler does not lower
/// to a `syntax-rules` macro (a procedural transformer, or any `define-syntax` that is not
/// a top-level form). Matched on the program parsed exactly as the compiler parses it,
/// every identifier, quoted or not, so a macro cannot assemble one from pieces. Returns
/// why. A program that does not parse is left to the compiler, which fails it the same way.
pub(crate) fn screen(src: &str) -> std::result::Result<(), String> {
    let parsed: std::result::Result<Vec<ExprKind>, _> = Parser::new(src, None)
        .without_lowering()
        .map(|expr| expr.and_then(lower_macro_and_require_definitions))
        .collect();
    let Ok(exprs) = parsed else {
        return Ok(());
    };
    exprs.iter().try_for_each(screen_expr)
}

fn refused(form: &str) -> String {
    format!(
        "`{form}` {REFUSAL_MARK}: it reaches outside the evaluation \
         (ikigai-lisp's sandbox, src/sandbox.rs)"
    )
}

fn screen_atom(ty: &TokenType<steel::parser::interner::InternedString>) -> Result<(), String> {
    match ty {
        TokenType::Require => Err(refused("require")),
        // A `define-syntax` keyword still standing after the compiler's own lowering is
        // one it did NOT turn into a pure `syntax-rules` macro: a procedural transformer,
        // a nested one, or one a template would assemble. The compiler hands those to its
        // macro engine, which evaluates them.
        TokenType::DefineSyntax => Err(refused("define-syntax (other than syntax-rules)")),
        TokenType::Identifier(s) => match s.resolve() {
            "define-syntax" => Err(refused("define-syntax (other than syntax-rules)")),
            name if REFUSED_FORMS.contains(&name) => Err(refused(name)),
            // The reader spells `,x` `,@x` and `'x` with these; they are syntax.
            "#%quote" | "#%unquote" | "#%unquote-splicing" | "#%unquote-comma" => Ok(()),
            // Steel's private names. A program never needs to write one (the allowed
            // macros expand INTO some, after this check), and `#%prim.` names are the
            // ones the compiler's constant folder evaluates from its own table at compile
            // time, which a rebinding cannot reach.
            name if name.starts_with("#%") || name.starts_with("##") => Err(format!(
                "`{name}` {REFUSAL_MARK}: it is one of Steel's private names \
                 (ikigai-lisp's sandbox, src/sandbox.rs)"
            )),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

fn screen_expr(e: &ExprKind) -> Result<(), String> {
    match e {
        ExprKind::Atom(a) => screen_atom(&a.syn.ty),
        ExprKind::If(f) => [&f.test_expr, &f.then_expr, &f.else_expr]
            .into_iter()
            .try_for_each(screen_expr),
        ExprKind::Let(l) => {
            for (name, value) in &l.bindings {
                screen_expr(name)?;
                screen_expr(value)?;
            }
            screen_expr(&l.body_expr)
        }
        ExprKind::Define(d) => {
            screen_expr(&d.name)?;
            screen_expr(&d.body)
        }
        ExprKind::LambdaFunction(l) => {
            l.args.iter().try_for_each(screen_expr)?;
            screen_expr(&l.body)
        }
        ExprKind::Begin(b) => b.exprs.iter().try_for_each(screen_expr),
        ExprKind::Return(r) => screen_expr(&r.expr),
        ExprKind::Quote(q) => screen_expr(&q.expr),
        ExprKind::Macro(m) => {
            screen_expr(&m.name)?;
            screen_rules(&m.syntax_rules)
        }
        ExprKind::SyntaxRules(r) => screen_rules(r),
        ExprKind::List(l) => l.args.iter().try_for_each(screen_expr),
        ExprKind::Set(s) => {
            screen_expr(&s.variable)?;
            screen_expr(&s.expr)
        }
        ExprKind::Require(_) => Err(refused("require")),
        ExprKind::Vector(v) => v.args.iter().try_for_each(screen_expr),
    }
}

fn screen_rules(rules: &steel::parser::ast::SyntaxRules) -> Result<(), String> {
    rules.syntax.iter().try_for_each(screen_expr)?;
    rules.patterns.iter().try_for_each(|pair| {
        screen_expr(&pair.pattern)?;
        screen_expr(&pair.body)
    })
}

#[cfg(test)]
pub(crate) mod derive {
    //! Derive the allowlist's second list from what code REFERENCES, never by running
    //! anything Steel exposes: the reference graph of the engine's compiled stdlib modules,
    //! read from their expanded ASTs, closed over from the names programs use.

    use std::collections::{BTreeMap, BTreeSet};
    use steel::parser::ast::ExprKind;
    use steel::parser::tokens::TokenType;
    use steel::steel_vm::engine::Engine;

    /// Every identifier an expression mentions outside quoted data and macro templates.
    /// Over-approximates (a local that shares a global's name counts), never under.
    pub fn idents(e: &ExprKind, out: &mut BTreeSet<String>) {
        match e {
            ExprKind::Atom(a) => {
                if let TokenType::Identifier(s) = &a.syn.ty {
                    out.insert(s.resolve().to_string());
                }
            }
            ExprKind::If(f) => {
                idents(&f.test_expr, out);
                idents(&f.then_expr, out);
                idents(&f.else_expr, out);
            }
            ExprKind::Let(l) => {
                for (_, b) in &l.bindings {
                    idents(b, out);
                }
                idents(&l.body_expr, out);
            }
            ExprKind::Define(d) => idents(&d.body, out),
            ExprKind::LambdaFunction(l) => idents(&l.body, out),
            ExprKind::Begin(b) => b.exprs.iter().for_each(|x| idents(x, out)),
            ExprKind::Return(r) => idents(&r.expr, out),
            ExprKind::List(l) => l.args.iter().for_each(|x| idents(x, out)),
            ExprKind::Set(s) => {
                idents(&s.variable, out);
                idents(&s.expr, out);
            }
            ExprKind::Quote(_)
            | ExprKind::Macro(_)
            | ExprKind::SyntaxRules(_)
            | ExprKind::Require(_)
            | ExprKind::Vector(_) => {}
        }
    }

    /// Every identifier anywhere in an UNEXPANDED expression, quoted data and macro
    /// templates included.
    pub fn all_idents(e: &ExprKind, out: &mut BTreeSet<String>) {
        match e {
            ExprKind::Atom(a) => {
                if let TokenType::Identifier(s) = &a.syn.ty {
                    out.insert(s.resolve().to_string());
                }
            }
            ExprKind::Quote(q) => all_idents(&q.expr, out),
            ExprKind::List(l) => l.args.iter().for_each(|x| all_idents(x, out)),
            ExprKind::Vector(v) => v.args.iter().for_each(|x| all_idents(x, out)),
            other => idents(other, out),
        }
    }

    /// The reference graph: a defined global's name to the names its body mentions.
    #[derive(Default)]
    pub struct Graph {
        /// Definitions inside the stdlib's modules (mangled names).
        pub defs: BTreeMap<String, BTreeSet<String>>,
        /// Top-level definitions, LAST definer wins, as at runtime: Steel's top-level
        /// `stdlib.scm`, then the module imports that overwrite the names a module
        /// provides (an edge to the module's mangled definition), then this crate's prelude.
        pub top: BTreeMap<String, BTreeSet<String>>,
        /// A module's mangled prefix to the module's path.
        pub modules: BTreeMap<String, String>,
        pub globals: BTreeSet<String>,
    }

    /// `(%proto-hash-get% __module-P 'X)`: an import, resolved when the module loaded, of
    /// another module's `X` — a reference to `P + X`, not to the whole export table.
    fn import_target(body: &ExprKind) -> Option<String> {
        let list = body.list()?;
        if list.first_ident()?.resolve() != "%proto-hash-get%" {
            return None;
        }
        let table = list.args.get(1)?.atom_identifier()?.resolve().to_string();
        let prefix = table.strip_prefix("__module-")?.to_string();
        match list.args.get(2)? {
            ExprKind::Quote(q) => Some(format!("{prefix}{}", q.expr.atom_identifier()?.resolve())),
            _ => None,
        }
    }

    /// `(%module-get% %-builtin-module-M 'X)`: a native fetched when the module loaded.
    fn is_native_import(body: &ExprKind) -> bool {
        body.list()
            .and_then(|l| l.first_ident())
            .map(|f| matches!(f.resolve(), "%module-get%" | "##__module-get"))
            .unwrap_or(false)
    }

    fn defines(e: &ExprKind, into: &mut BTreeMap<String, BTreeSet<String>>, replace: bool) {
        match e {
            ExprKind::Define(d) => {
                let Some(name) = d.name.atom_identifier() else {
                    return;
                };
                let mut refs = BTreeSet::new();
                if let Some(target) = import_target(&d.body) {
                    refs.insert(target);
                } else if !is_native_import(&d.body) {
                    idents(&d.body, &mut refs);
                }
                let name = name.resolve().to_string();
                if replace {
                    into.insert(name, refs);
                } else {
                    into.entry(name).or_default().extend(refs);
                }
            }
            ExprKind::Begin(b) => b.exprs.iter().for_each(|x| defines(x, into, replace)),
            _ => {}
        }
    }

    fn provided_names(provide: &ExprKind, out: &mut Vec<String>) {
        let Some(list) = provide.list() else { return };
        for item in list.args.iter().skip(1) {
            if let Some(name) = item.atom_identifier() {
                out.push(name.resolve().to_string());
            } else if let Some(spec) = item.list() {
                if let Some(name) = spec.args.get(1).and_then(|n| n.atom_identifier()) {
                    out.push(name.resolve().to_string());
                }
            }
        }
    }

    /// The graph of a built engine (before lockdown), plus the top-level programs that ran
    /// on it: Steel's own `stdlib.scm` at top level, and whatever `top_level` the caller
    /// names (this crate's prelude stages).
    pub fn graph(engine: &mut Engine, top_level: &[&str]) -> Graph {
        let mut graph = Graph {
            globals: engine
                .globals()
                .iter()
                .map(|s| s.resolve().to_string())
                .collect(),
            ..Graph::default()
        };
        let modules: Vec<_> = engine
            .modules()
            .iter()
            .map(|(path, module)| {
                (
                    path.to_string_lossy().to_string(),
                    module.prefix().to_string(),
                    module.get_ast().to_vec(),
                    module.get_compiled_ast().clone(),
                    module.get_provides().to_vec(),
                )
            })
            .collect();
        let mut provided = Vec::new();
        for (path, prefix, ast, compiled, provides) in modules {
            graph.modules.insert(prefix.clone(), path);
            for e in ast.iter().chain(compiled.iter()) {
                defines(e, &mut graph.defs, false);
            }
            let mut names = Vec::new();
            for p in &provides {
                provided_names(p, &mut names);
            }
            provided.extend(
                names
                    .into_iter()
                    .map(|n| (n.clone(), format!("{prefix}{n}"))),
            );
        }
        let mut top_level_defs = |text: &str, graph: &mut Graph| {
            if let Ok(exprs) = engine.emit_fully_expanded_ast(text, None) {
                for e in &exprs {
                    defines(e, &mut graph.top, true);
                }
            }
        };
        top_level_defs(steel::stdlib::PRELUDE, &mut graph);
        for (name, mangled) in provided {
            graph.top.insert(name, BTreeSet::from([mangled]));
        }
        for text in top_level {
            top_level_defs(text, &mut graph);
        }
        graph
    }

    /// The globals a program's expansion references.
    pub fn program_refs(
        engine: &mut Engine,
        globals: &BTreeSet<String>,
        src: &str,
    ) -> Result<BTreeSet<String>, String> {
        let exprs = engine
            .emit_fully_expanded_ast(src, None)
            .map_err(|e| e.to_string())?;
        let mut refs = BTreeSet::new();
        for e in &exprs {
            idents(e, &mut refs);
        }
        Ok(refs.into_iter().filter(|r| globals.contains(r)).collect())
    }

    /// Everything `seeds` reach through the graph.
    pub fn closure(graph: &Graph, seeds: &BTreeSet<String>) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<String> = seeds.iter().cloned().collect();
        while let Some(name) = stack.pop() {
            if !graph.globals.contains(&name) || !seen.insert(name.clone()) {
                continue;
            }
            let edges = graph.top.get(&name).or_else(|| graph.defs.get(&name));
            for next in edges.into_iter().flatten() {
                if !seen.contains(next) {
                    stack.push(next.clone());
                }
            }
        }
        seen
    }

    /// Spell a mangled name as `module-path::name`, which survives a change of prefix.
    pub fn stable(graph: &Graph, name: &str) -> String {
        for (prefix, path) in &graph.modules {
            if let Some(rest) = name.strip_prefix(prefix.as_str()) {
                return format!("{path}::{rest}");
            }
        }
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// Every global's value, by name — what a program referencing that name would read.
    fn values(engine: &Engine) -> BTreeMap<String, Option<SteelVal>> {
        let names: BTreeSet<String> = engine
            .globals()
            .iter()
            .map(|n| n.resolve().to_string())
            .collect();
        names
            .into_iter()
            .map(|name| {
                let value = engine.extract_value(&name).ok();
                (name, value)
            })
            .collect()
    }

    /// THE PIN. The names a program can reach after build — observed on the engine, as
    /// every global whose value lock-down left alone — EQUAL the allowlist (plus the
    /// lifted-function class). Not "contains no known bad name": a Steel upgrade that adds
    /// a global, or an allowlist entry that stops resolving, fails here.
    #[test]
    fn the_names_a_program_can_reach_equal_the_allowlist() {
        let mut engine = crate::build_unlocked_engine();
        let before = values(&engine);
        let expected = allowed(&engine);
        lock_down(&mut engine);
        let after = values(&engine);

        let mut reachable = BTreeSet::new();
        let mut lifted = 0;
        for (name, value) in &after {
            if before.get(name) != Some(value) {
                continue;
            }
            if name.starts_with(LIFTED_PREFIX) {
                // Kept as a class: each must be capture-free code (see LIFTED_PREFIX).
                match value {
                    Some(SteelVal::Closure(c)) => assert!(
                        c.captures().is_empty(),
                        "lifted function {name} captures values"
                    ),
                    other => panic!("lifted global {name} is not a closure: {other:?}"),
                }
                lifted += 1;
                continue;
            }
            reachable.insert(name.clone());
        }
        let expected: BTreeSet<String> = expected.into_iter().collect();
        let missing: Vec<_> = expected.difference(&reachable).collect();
        let extra: Vec<_> = reachable.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "the reachable names are not the allowlist\n  on the list, not reachable: \
             {missing:?}\n  reachable, not on the list: {extra:?}"
        );
        assert!(lifted > 0, "expected the stdlib's lifted functions");
        // Everything else was refused: the sandbox is the list, not the engine.
        assert!(
            after.len() > 10 * reachable.len(),
            "{} of {}",
            reachable.len(),
            after.len()
        );
    }

    /// Every allowlist entry names a global this Steel defines: an entry that stopped
    /// resolving would make a stdlib function refuse, silently.
    #[test]
    fn every_allowlist_entry_resolves() {
        let engine = crate::build_unlocked_engine();
        let globals: BTreeSet<String> = engine
            .globals()
            .iter()
            .map(|n| n.resolve().to_string())
            .collect();
        let allowed = allowed(&engine);
        let unresolved: Vec<_> = allowed.iter().filter(|n| !globals.contains(*n)).collect();
        assert!(
            unresolved.is_empty(),
            "allowlist entries with no global: {unresolved:?}"
        );
        let modules: usize = STDLIB_INTERNALS
            .iter()
            .filter(|n| matches!(n, In(..)))
            .count();
        let tops = PRELUDE_NAMES.len()
            + PROGRAM_NAMES.len()
            + STDLIB_INTERNALS
                .iter()
                .filter(|n| matches!(n, Top(_)))
                .count();
        assert_eq!(
            allowed.len(),
            tops + modules,
            "an In(..) entry names a missing module"
        );
    }

    /// Only the allowed macros remain in scope.
    #[test]
    fn only_the_allowed_macros_are_in_scope() {
        let mut engine = crate::build_unlocked_engine();
        lock_down(&mut engine);
        let in_scope: BTreeSet<String> = engine
            .in_scope_macros()
            .keys()
            .map(|k| k.resolve().to_string())
            .collect();
        let expected: BTreeSet<String> = MACROS.iter().map(|m| m.to_string()).collect();
        assert_eq!(in_scope, expected);
    }

    /// The derived list stays derived: closing the prelude's and the programs' names over
    /// the reference graph of THIS Steel's compiled stdlib yields exactly the allowlist.
    /// When Steel's internals move, this fails and says which way.
    #[test]
    fn the_internals_are_the_closure_of_the_program_names() {
        let mut engine = crate::build_unlocked_engine();
        let graph = super::derive::graph(&mut engine, &crate::PRELUDE_STAGES);
        let seeds: BTreeSet<String> = PRELUDE_NAMES
            .iter()
            .chain(PROGRAM_NAMES)
            .map(|n| n.to_string())
            .collect();
        let closure: BTreeSet<String> = super::derive::closure(&graph, &seeds)
            .iter()
            .filter(|n| !n.starts_with(LIFTED_PREFIX))
            .map(|n| super::derive::stable(&graph, n))
            .collect();
        let listed: BTreeSet<String> = seeds
            .iter()
            .cloned()
            .chain(STDLIB_INTERNALS.iter().map(|n| match n {
                Top(n) => n.to_string(),
                In(path, n) => format!("{path}::{n}"),
            }))
            .collect();
        let missing: Vec<_> = closure.difference(&listed).collect();
        let extra: Vec<_> = listed.difference(&closure).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "STDLIB_INTERNALS is not the closure\n  needed, not listed: {missing:?}\n  \
             listed, not needed: {extra:?}"
        );
    }

    #[test]
    fn the_screen_passes_ordinary_programs() {
        for src in [
            "(+ 1 2)",
            r#"(define-syntax m (syntax-rules () ((_ x) (list x x)))) (m 1)"#,
            r#"(with-handler (lambda (e) "caught") (error "boom"))"#,
            r#"(let* ((x 1) (y (+ x 1))) `(,x ,@(list y)))"#,
            "'(quoted data)",
            "(unclosed",
        ] {
            assert_eq!(screen(src), Ok(()), "{src}");
        }
    }

    #[test]
    fn the_screen_refuses_forms_that_leave_the_evaluation() {
        for (src, form) in [
            (r#"(require "steel/io")"#, "require"),
            (r#"(begin (require "x.scm"))"#, "require"),
            ("(require-builtin steel/process)", "require-builtin"),
            ("(defmacro (m) 1)", "defmacro"),
            ("(begin-for-syntax (define x 1))", "begin-for-syntax"),
            (
                "(define-syntax (m stx) (syntax-case stx () [(_) (syntax 1)]))",
                "define-syntax",
            ),
            ("(define-syntax m (lambda (stx) 1))", "define-syntax"),
            // A syntax-rules macro cannot assemble one either: the keyword in its template
            // is refused wherever it appears.
            (
                "(define-syntax mk (syntax-rules () ((_ n) (define-syntax n (lambda (s) 1)))))",
                "define-syntax",
            ),
            (
                "(define-syntax mk (syntax-rules () ((_) (defmacro (n) 1))))",
                "defmacro",
            ),
            ("(m defmacro)", "defmacro"),
            ("'require", "require"),
            (r#"(#%prim.string-ref "abc" 0)"#, "#%prim.string-ref"),
        ] {
            let why = screen(src).expect_err(src);
            assert!(
                why.contains(REFUSAL_MARK) && why.contains(form),
                "{src}: {why}"
            );
        }
    }
}

#[cfg(test)]
mod derive_tool {
    use super::derive;
    use std::collections::BTreeSet;

    /// The one-off derivation (not a gate): `IKIGAI_LISP_DERIVE=<dir>` names a directory of
    /// program files, one program each; this prints the globals their expansions reference
    /// and the closure of those through the stdlib's reference graph.
    #[test]
    #[ignore]
    fn derive_from_programs() {
        let dir = std::env::var("IKIGAI_LISP_DERIVE").expect("IKIGAI_LISP_DERIVE=<dir>");
        let mut engine = crate::build_unlocked_engine();
        let graph = derive::graph(&mut engine, &crate::PRELUDE_STAGES);
        let mut seeds = BTreeSet::new();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut scratch = crate::build_unlocked_engine();
            match derive::program_refs(&mut scratch, &graph.globals, &text) {
                Ok(refs) => seeds.extend(refs),
                Err(e) => println!("EXPAND ERROR {}: {e}", path.display()),
            }
        }
        let closure = derive::closure(&graph, &seeds);
        // Macros: identifiers in the UNEXPANDED programs that name an in-scope macro.
        let macro_names: BTreeSet<String> = engine
            .in_scope_macros()
            .keys()
            .map(|k| k.resolve().to_string())
            .collect();
        let mut used_macros = BTreeSet::new();
        for path in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
            let text = std::fs::read_to_string(&path).unwrap();
            if let Ok(exprs) = steel::steel_vm::engine::Engine::emit_ast(&text) {
                let mut ids = BTreeSet::new();
                for e in &exprs {
                    derive::all_idents(e, &mut ids);
                }
                used_macros.extend(ids.intersection(&macro_names).cloned());
            }
        }
        println!(
            "MACROS {}\n{}",
            used_macros.len(),
            used_macros.iter().cloned().collect::<Vec<_>>().join(" ")
        );
        let stable = |set: &BTreeSet<String>| -> Vec<String> {
            let mut v: Vec<String> = set.iter().map(|n| derive::stable(&graph, n)).collect();
            v.sort();
            v
        };
        println!("SEEDS {}\n{}", seeds.len(), stable(&seeds).join("\n"));
        let internal: BTreeSet<String> = closure.difference(&seeds).cloned().collect();
        println!(
            "INTERNAL {}\n{}",
            internal.len(),
            stable(&internal).join("\n")
        );
    }
}
