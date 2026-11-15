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

#[test]
fn sort_with_a_comparator_is_available() {
    for (src, expected) in [
        ("(sort (list 3 1 2) <)", "(1 2 3)"),
        ("(sort (list 3 1 2 1) (lambda (a b) (> a b)))", "(3 2 1 1)"),
        ("(sort (list) <)", "()"),
        ("(sort (list 1) (lambda (a b) (< a b)))", "(1)"),
        (
            "(define xs (list 3 1 2)) (list (sort xs <) xs)",
            "((1 2 3) (3 1 2))",
        ),
    ] {
        assert_eq!(eval(src).unwrap(), expected, "{src}");
    }
}

#[test]
fn string_split_is_available() {
    for (src, expected) in [
        (r#"(string-split "a,b,c" ",")"#, r#"("a" "b" "c")"#),
        (r#"(string-split ",a,," ",")"#, r#"("" "a" "" "")"#),
        (r#"(string-split "" ",")"#, r#"("")"#),
        (r#"(string-split "abc" ",")"#, r#"("abc")"#),
        (r#"(string-split "猫::犬::" "::")"#, r#"("猫" "犬" "")"#),
        (r#"(string-split "a.b.*c" ".")"#, r#"("a" "b" "*c")"#),
        (r#"(string-split "猫犬" "")"#, r#"("" "猫" "犬" "")"#),
        (r#"(split-many "a,b" ",")"#, r#"("a" "b")"#),
        (
            r#"(cacheable (sort (map string->number (string-split "3,1,2" ",")) <))"#,
            "(1 2 3)",
        ),
    ] {
        assert_eq!(eval(src).unwrap(), expected, "{src}");
    }
}

#[test]
fn shaping_functions_propagate_catchable_errors() {
    for src in [
        r#"(with-handler (lambda (e) "caught") (string-split 1 ","))"#,
        r#"(with-handler (lambda (e) "caught") (string-split "abc"))"#,
        r#"(with-handler (lambda (e) "caught") (sort (list 2 1) (lambda (a b) (error "bad comparison"))))"#,
    ] {
        assert_eq!(eval(src).unwrap(), "caught", "{src}");
    }
    refused(
        "(sort (list 2 1) (lambda (a b) (env-var \"HOME\")))",
        "env-var",
    );
}

/// Ledger #2720: division, roots, trigonometry and conversion are pure arithmetic.
#[test]
fn arithmetic_beyond_the_integers_is_available() {
    for (src, expected) in [
        ("(/ 7 2)", "7/2"),
        ("(/ 6 3)", "2"),
        ("(/ 7.0 2)", "3.5"),
        ("(exact->inexact (/ 1 4))", "0.25"),
        ("(sqrt 16)", "4"),
        ("(sqrt 2)", "1.4142135623730951"),
        ("(sin 0)", "0.0"),
        ("(cos 0)", "1.0"),
        ("(asin 1)", "1.5707963267948966"),
        ("(atan 1)", "0.7853981633974483"),
        ("(abs -3)", "3"),
        ("(min 3 1 2)", "1"),
        ("(max 1 2.5)", "2.5"),
        ("(floor 2.5)", "2.0"),
        ("(round 3.5)", "4.0"),
        ("(zero? 0)", "#true"),
        (r#"(with-handler (lambda (e) "caught") (/ 1 0))"#, "caught"),
    ] {
        assert_eq!(eval(src).unwrap(), expected, "{src}");
    }
}

/// The rung chapter 8 of the ikigai Book could not climb: a great-circle distance
/// (haversine), London to New York, in kilometers.
#[test]
fn a_great_circle_distance_is_computable() {
    let src = "(define (rad d) (* d (/ 3.141592653589793 180)))
        (define (sq x) (* x x))
        (define (distance lat1 lon1 lat2 lon2)
          (let* ((a (+ (sq (sin (/ (rad (- lat2 lat1)) 2)))
                       (* (cos (rad lat1)) (cos (rad lat2))
                          (sq (sin (/ (rad (- lon2 lon1)) 2)))))))
            (* 2 6371 (asin (sqrt a)))))
        (round (distance 51.5007 0.1246 40.6892 -74.0445))";
    assert_eq!(eval(src).unwrap(), "5591.0");
}

/// Ledger #2720: the helpers chapter 8 hand-wrote (`last`, `first-n`, `join-lines`, an
/// assoc tally) are Steel's own.
#[test]
fn folds_slices_joins_and_tables_are_available() {
    for (src, expected) in [
        (r#"(string-join (list "a" "b" "c") "\n")"#, "a\nb\nc"),
        (r#"(string-join (list "a" "b"))"#, "ab"),
        (r#"(string-join (list) ",")"#, ""),
        ("(foldl + 0 (list 1 2 3))", "6"),
        ("(foldl cons '() (list 1 2 3))", "(3 2 1)"),
        ("(foldr cons '() (list 1 2 3))", "(1 2 3)"),
        ("(last (list 1 2 3))", "3"),
        ("(take (list 1 2 3) 2)", "(1 2)"),
        ("(take (list 1 2 3) 5)", "(1 2 3)"),
        ("(drop (list 1 2 3) 2)", "(3)"),
        ("(hash-try-get (hash 'a 1) 'a)", "1"),
        ("(hash-try-get (hash 'a 1) 'b)", "#false"),
        ("(hash-contains? (hash 'a 1) 'a)", "#true"),
        ("(hash->list (hash-insert (hash) 'a 1))", "((a . 1))"),
        // A table is a value: inserting answers a new one and leaves the old alone.
        (
            "(define h (hash 'a 1)) (hash-insert h 'b 2) (hash->list h)",
            "((a . 1))",
        ),
        // A tally, as chapter 8's program keeps one.
        (
            "(define (tally xs)
               (foldl (lambda (x h) (hash-insert h x (+ 1 (or (hash-try-get h x) 0))))
                      (hash) xs))
             (sort (hash->list (tally (list 'b 'a 'b)))
                   (lambda (p q) (> (cdr p) (cdr q))))",
            "((b . 2) (a . 1))",
        ),
    ] {
        assert_eq!(eval(src).unwrap(), expected, "{src}");
    }
    for src in [
        r#"(with-handler (lambda (e) "caught") (string-join (list "a" 1) ","))"#,
        r#"(with-handler (lambda (e) "caught") (last (list)))"#,
        r#"(with-handler (lambda (e) "caught") (take (list 1 2) -1))"#,
        r#"(with-handler (lambda (e) "caught") (drop (list 1 2) 5))"#,
    ] {
        assert_eq!(eval(src).unwrap(), "caught", "{src}");
    }
    // A fold's function runs in the same sandbox as the rest of the program.
    refused(
        r#"(foldl (lambda (x acc) (env-var "HOME")) 0 (list 1))"#,
        "env-var",
    );
    // An exact power allocates out of proportion to its input, so it stays refused.
    refused("(expt 2 3)", "expt");
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
