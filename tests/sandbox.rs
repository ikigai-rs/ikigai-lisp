//! A program holding only `urn:cap:lisp` reaches the world through the kernel verbs and
//! nothing else (audit round 6, ledger #903: H-F2, C-R1–R4).
//!
//! Each test takes the audit reproduction's INPUT and asserts the REFUSAL: the form is
//! refused before it compiles, or the name is bound to a refusal. None of them reaches the
//! host — nothing here executes a host program, reads a file, or touches the network — so
//! a passing test is the sandbox refusing, and a failing one says which door opened.
//! Every refusal is a typed, permanent `Denied`.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};

fn eval(src: &str) -> Result<String, Error> {
    let kernel = Kernel::new(Arc::new(ikigai_lisp::space()));
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:lisp:eval").expect("valid IRI"),
    )
    .with_arg("in", ArgRef::Inline(src.as_bytes().to_vec()));
    block_on(kernel.issue(request, &Capability::scoped(["urn:cap:lisp"])))
        .map(|repr| String::from_utf8(repr.bytes).expect("utf-8"))
}

/// Assert `src` is refused as `Denied`, naming `what`.
fn refused(src: &str, what: &str) {
    match eval(src) {
        Err(Error::Denied(why)) => assert!(
            why.contains(what) && why.contains("is not available to a program"),
            "{src}: refused, but the refusal does not name `{what}`: {why}"
        ),
        other => panic!("{src}: expected a Denied refusal naming `{what}`, got {other:?}"),
    }
}

/// H-F2: `(require "<path>")` resolved string literals to filesystem paths, so a program
/// could load and run any file the service user could read. Refused before compilation,
/// whatever the path — absolute, a module spec, or a builtin module's name.
#[test]
fn h_f2_require_of_any_path_is_refused() {
    for src in [
        r#"(require "/tmp/audit-cog.scm") (if (defined? audit-cog-fn) (audit-cog-fn) "probe-missing")"#,
        r#"(require "/etc/hosts")"#,
        r#"(require (only-in "/tmp/ikigai-audit-req/mod1.scm" mod1-value)) mod1-value"#,
        r#"(require "steel/io") (if (defined? slurp) (slurp "/etc/hosts") "slurp-missing")"#,
        r#"(require "steel/threads") (if (defined? spawn) "has-spawn" "no-spawn")"#,
        r#"(require (lib "steel/time")) (if (defined? current-time) "has-time" "no-time")"#,
    ] {
        refused(src, "require");
    }
}

/// C-R1: `env-var` read the host's environment. The name is bound to a refusal.
#[test]
fn c_r1_env_var_is_refused() {
    refused(r#"(env-var "HOME")"#, "env-var");
}

/// C-R2: `command-line` read the host process's argv.
#[test]
fn c_r2_command_line_is_refused() {
    refused("(command-line)", "command-line");
}

/// C-R3: the `steel/process` module loaded in the sandbox. `require-builtin` is refused
/// before compilation, and the module's names, which Steel binds at top level anyway, are
/// refusals — `which` never reaches a PATH lookup.
#[test]
fn c_r3_steel_process_is_refused() {
    refused(
        r#"(require-builtin steel/process) (which "sh")"#,
        "require-builtin",
    );
    refused(r#"(which "sh")"#, "which");
}

/// C-R4: the `steel/git` module, whose `git-clone` reaches the network and the filesystem.
#[test]
fn c_r4_steel_git_is_refused() {
    refused("(require-builtin steel/git) git-clone", "require-builtin");
}

/// A refusal is an ordinary error inside the program — catchable like a denied verb — and
/// typed `Denied` when the program leaves it uncaught.
#[test]
fn a_refused_name_is_catchable() {
    assert_eq!(
        eval(r#"(with-handler (lambda (e) "caught") (env-var "HOME"))"#).unwrap(),
        "caught"
    );
}

/// There is no default input port: the host's stdin is not a program's (an MCP stdio host
/// carries its protocol there). `read` takes a string port, which is all a program can open.
#[test]
fn there_is_no_default_input_port() {
    let err = eval("(read)").expect_err("(read) with no port must fail");
    assert!(
        err.to_string().contains("no default input port"),
        "the error says what to do instead: {err}"
    );
    assert_eq!(
        eval(r#"(read (open-input-string "(a b) c"))"#).unwrap(),
        "(a b)"
    );
}

/// Program code never runs at expansion time: the compiler's macro engine is a second
/// Steel engine this crate cannot lock down, so the forms that reach it are refused.
#[test]
fn code_never_runs_at_expansion_time() {
    refused("(defmacro (m) 1) (m)", "defmacro");
    refused("(begin-for-syntax (define x 1))", "begin-for-syntax");
    refused(
        "(define-syntax (m stx) (syntax-case stx () [(_) (syntax 1)])) (m)",
        "define-syntax",
    );
    // `syntax-rules` is pattern rewriting, not evaluation: still available.
    assert_eq!(
        eval("(define-syntax twice (syntax-rules () ((_ x) (list x x)))) (twice 1)").unwrap(),
        "(1 1)"
    );
}

/// The compiler folds calls to Steel's pure primitives at compile time from its own table,
/// by their private `#%prim.` names — a path a rebinding cannot reach. Programs may not
/// write Steel's private names.
#[test]
fn steels_private_names_are_refused() {
    refused(r#"(#%prim.string-ref "abc" 0)"#, "#%prim.string-ref");
}
