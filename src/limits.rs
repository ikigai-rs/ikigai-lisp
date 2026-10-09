//! The bounds on an evaluation, and how a host sets them (audit round 6, ledger #903
//! C-B4–C-B7; #87; #214).
//!
//! Steel's front end — reader, macro expander, compiler — recurses once per level of
//! nesting, and so did this crate's adapter from a Lisp value to an s-expression. A
//! stack overflow is not a panic: it aborts the whole host process. So nesting is
//! bounded BEFORE Steel or the adapter sees it — on program text, on data a program
//! `read`s, and on a value crossing into `(graph …)`/`(sparql-select …)` — and each
//! worker thread gets a stack sized for the deepest input those bounds admit, with
//! headroom for what macro expansion adds (Steel caps that at 512 nested expansions).
//! Every bound refuses with a typed error; none truncates.
//!
//! A host sets them with [`set_limits`], once, before the first evaluation — from its
//! config home or its flags, never from environment variables.

use std::sync::OnceLock;

use steel::parser::lexer::TokenStream;
use steel::parser::tokens::TokenType;

/// The bounds on evaluation. Start from [`Limits::default`] and adjust with the
/// builder methods; hand the result to [`set_limits`].
///
/// ```
/// let limits = ikigai_lisp::Limits::default().workers(4).max_nesting(256);
/// assert_eq!(limits.max_nesting, 256);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The most eval worker threads alive at once, busy or idle. At the ceiling a new
    /// eval is refused with a transient `Unavailable`. Default: the host's available
    /// parallelism, at least 8.
    pub workers: usize,
    /// The stack each worker thread reserves, in bytes. Reserved address space, not
    /// memory: pages are committed as the stack grows. Default: 128 MiB. Measured
    /// 2026-10-09 (steel-core 0.8.3, Apple silicon): the deepest programs the default
    /// [`max_nesting`](Self::max_nesting) admits — 990 levels of text around a macro
    /// chain near Steel's expansion cap — compile in 32 MiB with Steel unoptimized and
    /// in under 32 MiB optimized, so the default holds four times that.
    pub worker_stack_bytes: usize,
    /// The largest program text, in bytes. Default: 4 MiB.
    pub max_program_bytes: usize,
    /// The largest `(input)` data handed to a program, in bytes. Default: 16 MiB.
    pub max_input_bytes: usize,
    /// The deepest nesting accepted in program text, in data a program `read`s, and in
    /// a value passed to `(graph …)` or `(sparql-select …)`. Each list, vector and
    /// quote counts one level. Default: 1,000.
    pub max_nesting: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            workers: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8)
                .max(8),
            worker_stack_bytes: 128 * 1024 * 1024,
            max_program_bytes: 4 * 1024 * 1024,
            max_input_bytes: 16 * 1024 * 1024,
            max_nesting: 1_000,
        }
    }
}

impl Limits {
    /// Set [`workers`](Self::workers) (at least 1).
    pub fn workers(mut self, workers: usize) -> Self {
        self.workers = workers.max(1);
        self
    }

    /// Set [`worker_stack_bytes`](Self::worker_stack_bytes).
    pub fn worker_stack_bytes(mut self, bytes: usize) -> Self {
        self.worker_stack_bytes = bytes;
        self
    }

    /// Set [`max_program_bytes`](Self::max_program_bytes).
    pub fn max_program_bytes(mut self, bytes: usize) -> Self {
        self.max_program_bytes = bytes;
        self
    }

    /// Set [`max_input_bytes`](Self::max_input_bytes).
    pub fn max_input_bytes(mut self, bytes: usize) -> Self {
        self.max_input_bytes = bytes;
        self
    }

    /// Set [`max_nesting`](Self::max_nesting).
    pub fn max_nesting(mut self, depth: usize) -> Self {
        self.max_nesting = depth;
        self
    }
}

static LIMITS: OnceLock<Limits> = OnceLock::new();

/// Fix the bounds for this process. Call it once, before the first evaluation (the
/// worker pool and every engine read them). Returns the bounds already in force if
/// they were fixed first — by an earlier call, or by an evaluation that ran with the
/// defaults.
pub fn set_limits(limits: Limits) -> Result<(), Limits> {
    let mut offered = Some(limits);
    let fixed = LIMITS.get_or_init(|| offered.take().expect("taken once"));
    match offered {
        None => Ok(()),
        Some(_) => Err(fixed.clone()),
    }
}

/// The bounds in force, fixing the defaults if no host set any.
pub fn limits() -> &'static Limits {
    LIMITS.get_or_init(Limits::default)
}

/// How deeply `text` nests, read with Steel's own lexer: each open bracket, and each
/// quote-like prefix (`'`, `` ` ``, `,`, `,@`, `#'`, …, and `#;`), is one level for the
/// datum it opens. Never recurses, so it is safe on any input. A lexing error ends the
/// scan; the parser fails the same text itself.
pub(crate) fn nesting(text: &str) -> usize {
    // A frame per open level: `true` for a bracket, `false` for a prefix waiting for
    // its datum. A datum that completes closes every prefix frame on top of it.
    let mut frames: Vec<bool> = Vec::new();
    let mut deepest = 0;
    let complete = |frames: &mut Vec<bool>| {
        while frames.last() == Some(&false) {
            frames.pop();
        }
    };
    for token in TokenStream::new(text, false, None) {
        let Ok(token) = token else { break };
        match token.ty {
            TokenType::Comment => {}
            TokenType::OpenParen(..) => frames.push(true),
            TokenType::CloseParen(_) => {
                while let Some(frame) = frames.pop() {
                    if frame {
                        break;
                    }
                }
                complete(&mut frames);
            }
            TokenType::QuoteTick
            | TokenType::QuasiQuote
            | TokenType::Unquote
            | TokenType::UnquoteSplice
            | TokenType::QuoteSyntax
            | TokenType::QuasiQuoteSyntax
            | TokenType::UnquoteSyntax
            | TokenType::UnquoteSpliceSyntax
            | TokenType::DatumComment => frames.push(false),
            _ => complete(&mut frames),
        }
        deepest = deepest.max(frames.len());
    }
    deepest
}

/// Refuse `text` past [`Limits::max_program_bytes`] or [`Limits::max_nesting`],
/// saying which bound and by how much.
pub(crate) fn check_program(text: &str) -> Result<(), String> {
    let limits = limits();
    if text.len() > limits.max_program_bytes {
        return Err(format!(
            "the program is {} bytes; the bound is {} (ikigai-lisp Limits::max_program_bytes)",
            text.len(),
            limits.max_program_bytes
        ));
    }
    check_nesting("the program", text)
}

/// Refuse `text` nested past [`Limits::max_nesting`].
pub(crate) fn check_nesting(what: &str, text: &str) -> Result<(), String> {
    let max = limits().max_nesting;
    let depth = nesting(text);
    if depth > max {
        return Err(format!(
            "{what} nests {depth} levels deep; the bound is {max} (ikigai-lisp Limits::max_nesting)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::nesting;

    #[test]
    fn nesting_counts_brackets_and_quote_prefixes() {
        assert_eq!(nesting("1"), 0);
        assert_eq!(nesting("(+ 1 2)"), 1);
        assert_eq!(nesting("(a (b [c {d}]))"), 4);
        assert_eq!(nesting("'x"), 1);
        assert_eq!(nesting("''''x"), 4);
        assert_eq!(nesting("'(a '(b))"), 4);
        assert_eq!(nesting("`(a ,(b ,@(c)))"), 6);
        assert_eq!(nesting("#(1 #(2))"), 2);
        // A prefix closes with its datum: siblings do not accumulate.
        assert_eq!(nesting("('a 'b 'c 'd)"), 2);
    }

    #[test]
    fn nesting_ignores_brackets_in_strings_comments_and_characters() {
        assert_eq!(nesting(r#"(display "((((((")"#), 1);
        assert_eq!(nesting("; ((((((\n(a)"), 1);
        assert_eq!(nesting("#| (((( |# (a)"), 1);
        assert_eq!(nesting(r"(list #\( #\))"), 1);
    }
}
