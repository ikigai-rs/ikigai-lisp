# nREPL for ikigai: a client for remote runtimes, a door for Clojure editors

**Status:** design spike, ledger #3162, 2026-11-05. This note is research and a design. It
adds no code and no dependency, and changes nothing this crate does. Brian asked for the
foundation under David's Lisp work: "Can we start working on the foundational tasks to
bootstrap David's work?"

Two pieces of that work speak [nREPL](https://nrepl.org). nREPL sends bencode dictionaries
over a stream. A request carries `op` and `id`, and every answer to it carries the same `id`.
A request is finished when an answer's `status` contains `done`. A server answers an op it
does not know with the status `unknown-op`. nREPL has no authentication of its own. Its
documentation calls a server on a public address "an epic security hole", so the port has to
be treated like a shell.

1. **A client.** `urn:lisp:eval:runtime:{runtime}` sends a program to a Clojure runtime
   that runs an nREPL server (babashka, jank or JVM Clojure) and answers with the value.
2. **A door.** CIDER, Calva and Conjure connect to an ikigai host. Their evals run as
   `urn:lisp:eval`, under the capability the host granted to the connection.

This note tries to take the risk out of both before anyone builds them. Every fact below has
a source and the date it was read, or says that it was run here and how. Anything that could
not be checked is marked as unchecked.

## Contents

- [What was found](#what-was-found)
- [Resource map](#resource-map)
- [Piece 1: the client](#piece-1-the-client)
- [Piece 2: the door](#piece-2-the-door)
- [Authority rules](#authority-rules)
- [Bencode: which crate](#bencode-which-crate)
- [Test plan](#test-plan)
- [Open questions for Brian](#open-questions-for-brian)
- [Sources](#sources)

## What was found

### nrepl-steel is a reference, not a library

[nrepl-steel](https://github.com/waddie/nrepl-steel) is Tom Waddington's nREPL server for
Steel, the Scheme this crate embeds. It lives in its own repository, not in Steel's: Steel's
tree at `498eacfc41b2` (2026-10-25) holds no nREPL or bencode code. Its closest piece is
`cogs/repl/repl.scm`, a plain socket REPL that binds `0.0.0.0`. Read 2026-11-05, at
`d412c6533cad` (2026-10-24):

- **License:** MIT (`LICENSE`, and `license = "MIT"` in `Cargo.toml`).
- **Maintenance:** one author, 17 commits since 2026-06-12, 4 stars, no open issues. Releases
  run from v0.2.0 to v0.4.0 (2026-10-24). It is **not on crates.io**: it installs through
  Steel's package manager (`forge pkg install --git …`). It pins `steel-core` to a git
  revision of 0.8.3.
- **Ops** (`nrepl-server/dispatch.scm:26-30`): `clone`, `describe`, `eval`, `load-file`,
  `close`, `ls-sessions`, `interrupt`, `completions`, `lookup` and `info` (an alias for
  `lookup`). It answers an unknown op with `status ["error" "unknown-op" "done"]`
  (`dispatch.scm:69-72`).
- **Gaps:**
  - `interrupt` is a stub that always answers `session-idle` (`dispatch.scm:206-215`; its
    README says a running eval "cannot be aborted");
  - there is no `stdin` op and no `need-input`;
  - output is sent once per eval, not streamed;
  - strings must be UTF-8;
  - one session driven from two connections at once is unsupported (`server.scm:129-131`);
  - the transport is TCP only, with no Unix socket.
- **No sandbox:** each session is a full `Engine::new()` (`engine.rs:96`), with filesystem,
  network and process access.
- **Its protocol half is Steel Scheme, not Rust.** The bencode codec (`bencode.scm`, 166
  lines), the transport, the sessions and the dispatch all run inside a Steel engine. The only
  Rust is the evaluator, loaded as a dylib over Steel's FFI, which is the half ikigai would
  replace.

**Verdict:** it cannot be the door's protocol half. A Rust door would have to run an
unsandboxed Steel engine just to speak the protocol, which is the opposite of this crate's
sandbox. It is worth reading as a reference, for:

- the status tokens it uses: `unknown-op`, `no-code`, `unknown-session`, `session-idle`,
  `eval-error`;
- the client quirks it handles: Calva's eval without a session, `info` sending `symbol`
  instead of `sym`, and CIDER reading `err`;
- its five-procedure evaluator interface (`evaluator.scm:338`);
- its tests (`test/dispatch-test.scm`, `test/integration-client.scm`).

Its README says it has been used with CIDER and Calva. That is evidence that a Scheme behind
a Clojure editor is workable. It was not run here.

### Babashka's server, run here

Run on 2026-11-05 with babashka v1.13.229, the latest release, published 2026-10-27. The binary
was the static linux-amd64 build. Its SHA-256,
`64fb984991c6e68f44e8a9391176a9bb411f9b9ae9959a6bca5307c75a7414da`, matched the published
`.sha256` file. The server was started with `bb nrepl-server 1667` and driven by a 90-line
Python bencode client, written for this spike and not committed. Results:

- **`describe`** reported nREPL 1.8.0, Clojure `1.12.6-SCI` and Java 25.0.4. It listed 89 ops:
  - nREPL's own: `clone`, `close`, `completions`, `describe`, `eval`, `interrupt`,
    `load-file`, `lookup`, `ls-sessions` and `stdin`;
  - babashka's: `forward-system-output`;
  - a CIDER subset, each also under a `cider/` prefix, with `cider-version` 0.63.1 in `aux`:
    `complete`, `info`, `eldoc`, `ns-list`, `classpath`, `stacktrace` and
    `analyze-last-stacktrace`, `test`, `test-all`, `test-var-query`, `retest` and
    `test-stacktrace`, the `inspect-*` family, the debugger ops (`init-debugger`,
    `debug-input`, `debug-instrumented-defs`), and `clojuredocs-lookup` with
    `clojuredocs-refresh-cache`.
- **`eval`** of `(+ 1 2)` answered `value "3"`, then `status ["done"]`. A request with three
  top-level forms got `out`, `err`, one `value` per form, and then `done`.
- **`eval`** of `(read-line)` answered `status ["need-input"]`. A `stdin` op with `typed\n`
  was acknowledged with `done`, and the eval then answered `value "\"typed\""`.
- **`eval`** of `(/ 1 0)` answered `err` with
  `java.lang.ArithmeticException: Divide by zero [at NO_SOURCE_PATH:1:1]`, then
  `ex`, `root-ex` and `status ["eval-error"]`, then `done`.
- **An unknown op** (`frobnicate`) answered `{"op": "frobnicate", "status": ["done",
  "unknown-op", "error"]}`.
- **`interrupt`** of a running `(loop [] (recur))` answered `status ["done" "interrupted"]` for
  the eval and `done` for the interrupt.
- **Late messages:** `status ["state"]` messages for finished requests arrived while later
  requests ran.
- **Reach:** `(subs (slurp "/etc/os-release") 0 12)` answered `"NAME=\"Pop!_O"`, and
  `(:exit (babashka.process/shell {:out :string} "true"))` answered `0`. An eval reaches the
  disk and starts processes, with nothing asked of the caller.
- **Bind address:** `bb nrepl-server 0.0.0.0:1668` listened on every interface (`ss -ltn`
  showed `*:1668`).
- **Deep input:** a bare list nested 10,000 and then 200,000 deep was read and refused as
  "nREPL message must be a map". The connection closed, and the server kept answering. Babashka
  survived this. The point of [the pre-scan](#bencode-which-crate) is that a Rust decoder must
  not be what decides whether it survives.

### Op coverage

The babashka and nREPL columns come from the run above, from the nREPL docs (now at
`docs.nrepl.org`) and from the spec draft at `spec.nrepl.org`. The jank, CIDER, Calva and
Conjure columns come from their source, read 2026-11-05 at pinned commits (see
[Sources](#sources)); none of those three editors was run here.

Key: **R**, required at connect or for any eval; **U**, used when `describe` lists it;
**S**, sent without checking `describe`; **F**, a fallback when the CIDER name is missing;
**—**, not implemented.

| op | spec draft | bb 1.13.229 (run) | jank | CIDER | Calva | Conjure | the door |
|---|---|---|---|---|---|---|---|
| `describe` | required | yes | yes | R | R | R | yes |
| `eval` | required | yes | yes | R | R | R | yes |
| `stdin` | required | yes | — | S | U | S on `need-input` | yes (see [the door](#piece-2-the-door)) |
| `clone` | optional | yes | yes, holding no state | R (twice) | R | R if no session | yes |
| `close` | optional | yes | handled, not listed | S | U | S | yes |
| `interrupt` | optional | yes | — | S | U | S | yes |
| `load-file` | optional | yes | yes | S | S | S | yes |
| `ls-sessions` | — | yes | — | S | U | S at connect; a failure is tolerated | yes |
| `completions` | optional | yes | yes | F | not used | U | yes |
| `lookup` | optional | yes | yes | F | not used | U | yes |
| `complete` (CIDER's) | — | yes | — | U | U | U, preferred | yes, as `completions` |
| `info` (CIDER's) | — | yes | — | U | U; warns when missing | U, preferred | yes, as `lookup` |
| `eldoc` | — | yes | — | U | — | — | yes, from the same contract |
| everything else | — | many | `unknown-op` | U | U | — | `unknown-op` |

What the table says:

- **The minimum is small.** For all three editors to connect and evaluate, a server needs
  `describe`, `clone`, `eval` and `close`, plus `ls-sessions` for Conjure; `load-file` and
  `interrupt` are sent without checking. The spec draft names `describe`, `eval` and `stdin`
  as the absolute minimum. CIDER now raises an error only for ops whose names contain a `/`
  (`cider-client.el`, lines 240-280); core ops pass through unchecked.
- **To be usable rather than merely connected,** a server also needs `complete` and `info`,
  the CIDER names. Calva sends only those two names, and only when `describe` lists them. CIDER
  and Conjure try them first and fall back to `completions` and `lookup`. The door lists all
  four.
- **`describe` must send `ops` as a dictionary, not a list.** The spec draft now recommends a
  list, but CIDER (`nrepl-dict-get nrepl-ops op`), Calva (`ops[op]`) and Conjure
  (`get-in [:describe :ops op]`) all look an op up as a key. Sent as a list, every op would look
  unsupported to every editor.
- **jank** runs its nREPL server inside `jank repl`, on a background thread. The server is
  written in jank (`compiler+runtime/src/jank/jank/nrepl/server/`) on Boost.Asio TCP. It listens
  on loopback only, and writes `.nrepl-port`. It lists `clone`, `describe`, `eval`,
  `load-file`, `lookup` and `completions`. It has no `stdin` or `interrupt`, and its `clone`
  keeps no session state. Its book says it "has been tested to work with both CIDER and
  Conjure". It installs from an apt repository for Ubuntu 24.04 and later (`ppa.jank-lang.org`,
  about 72 MB, with `libboost-all-dev`), from Homebrew on Apple silicon only, and from the AUR.
  It was not installed here.
- **Babashka changed recently.** Since v1.13.221 (2026-09-14), `bb nrepl-server` runs nREPL's
  own server from bundled source, not the older `babashka.nrepl` library, and binds `127.0.0.1`
  by default where it used to bind `0.0.0.0`. The library's `describe` lists neither `stdin`
  nor `interrupt`, and the version run here lists both. A test must pin the babashka version.

### What the editors send that is not an op

Each editor also *evaluates Clojure* as it connects. This was read in their source (CIDER
`cider-repl.el` lines 158-163 and 299-345; Calva `nrepl/index.ts` lines 255-325; Conjure
`server.fnl` lines 167-400). The forms were then run here against `urn:lisp:eval` itself,
through a scratch example that was deleted afterwards. All four came back as an ordinary
`Endpoint` error, a `FreeIdentifier`:

| editor | what it evaluates | when | `urn:lisp:eval` answered |
|---|---|---|---|
| Calva | `*ns*` | before `clone`, without a session, and it waits for `done` | `FreeIdentifier … *ns*` |
| Conjure | `#?(:clj 'clj :cljs 'cljs …)` | after `clone`, to learn the session's type | `FreeIdentifier … #?` |
| CIDER | `(when-let [requires (resolve 'clojure.main/repl-requires)] …)` | after `clone`; the rest of CIDER's setup runs in its callback | `FreeIdentifier … resolve` |
| Conjure | `(clojure.repl/doc map)` | for documentation, before it falls back to `info` | `FreeIdentifier … clojure.repl/doc` |

So the sandbox survives every handshake, and the door can answer each form with `eval-error`
and `done`, which CIDER and Calva wait for. Whether each editor carries on after that error was
not tested. Two things follow:

- this crate appends its isolation hint to each of those errors ("if it was defined in an
  earlier evaluation…"), which is wrong for an editor's probe. The door should answer the
  known handshake forms itself ([open question 5](#open-questions-for-brian));
- `(define (sq x) (* x x)) (sq 7)` answered `49`, but a `define` in one eval is not in scope in
  the next. An editor user expects it to be ([open question 2](#open-questions-for-brian)).

## Resource map

This map follows the ecosystem's resource-first method (`docs/resource-design.md` in
`ikigai-gonk-starter`). Each name is singular. The left-most element after `urn:lisp:` is
the set's type, and each level below it partitions the set by a named key.

| set | name | kind | how |
|---|---|---|---|
| runtime | `urn:lisp:runtime` | composition | a list of names: one `urn:lisp:runtime:name:{runtime}` per runtime the host declared |
| one runtime | `urn:lisp:runtime:name:{runtime}` | **atom (host configuration)** | what the host declared for this runtime: its kind (`babashka`, `jank`, `clojure`), the command, the sandbox profile, the capability it requires and its bounds. The host's config home writes it. No program or peer can Sink it. |
| runtime description | `urn:lisp:describe:runtime:{runtime}` | **code (connector)** | the runtime's own `describe` answer: its ops and versions, as JSON or Turtle. It is cacheable until the runtime's process is replaced, and is threaded on `urn:lisp:runtime:name:{runtime}`. |
| eval on a runtime | `urn:lisp:eval:runtime:{runtime}` | **code (connector)** | sends `clone`, `eval` and `close`, and answers with the last value. This is the one place that reaches a process, and it is uncacheable. |
| eval (this crate, today) | `urn:lisp:eval` | code (exists) | the Steel sandbox. Unchanged. The door evaluates through it. |
| a name's contract | the Meta face of any IRI | composition (exists) | `Description` and `ArgSpec`s rendered by the kernel. The door's `lookup` and `info` read it. |
| action | `urn:kernel:actions` | composition (exists) | what the capability may invoke. The door's completions read it. |
| nREPL door | `urn:lisp:door:nrepl` | **code (edge)** | the listener. It is not a resource a program can source. It describes itself through Meta, as the HTTP door does. |
| door grant | `urn:lisp:grant:fingerprint:{sha256}` | **atom (host configuration)** | maps a client certificate's SPKI SHA-256 fingerprint to a principal IRI and a capability ceiling. The host's config home writes it. The door only reads it. |
| door session | `urn:lisp:session:id:{id}` | **atom (door memory)** | an editor's nREPL session: its id and the definitions it has evaluated (see [open question 2](#open-questions-for-brian)). It lives as long as the connection. A session is never shared between principals. |

**What holds state:** the runtime declarations and the door grants, which are host
configuration and change only when the host's operator changes them, and the door sessions,
which live in memory for the life of a connection. Nothing else stores anything.

**What is composed:** the runtime list, every `lookup` and `completions` answer (from Meta and
`urn:kernel:actions`), and every door eval (from `urn:lisp:eval`). The door holds no evaluator
of its own.

**Code that cannot be a composition, and why:**

- **The connector** behind `urn:lisp:eval:runtime:*` and `urn:lisp:describe:runtime:*`. It
  starts an OS process and speaks a wire protocol to it, which no composition can do. It is
  the one named exception to Hypothesis H in this design (rule C2).
- **The door's listener.** It accepts connections and authenticates them, which no
  composition can do either. It is an edge in the same sense as `ikigai-web`'s HTTP door: it
  turns a peer into a principal and a capability, then issues requests into the kernel.
- **The bencode codec.** It is a pure function from bytes to dictionaries, shared by both
  pieces.

**Spaces.** A door session is the one candidate for a corridor: if sessions keep definitions
([open question 2](#open-questions-for-brian)), those definitions are the session's context, and
they belong in a corridor, not in an argument threaded through each eval. The runtime processes
are not spaces, because they hold no names the kernel resolves. The sealed names are
`urn:lisp:runtime:name:*` and `urn:lisp:grant:*`, which no module may answer for, since they
carry authority.

**Faces.** `urn:lisp:eval:runtime:{runtime}` answers `text/plain`, the value as the runtime
printed it. `urn:lisp:describe:runtime:{runtime}` answers `application/json` and `text/turtle`.
The door answers bencode on its socket. In the kernel it has only its Meta face.

## Piece 1: the client

### `urn:lisp:eval:runtime:{runtime}`

The item names this resource `urn:lisp:{runtime}:eval`. That spelling puts an unkeyed variable
ahead of the type, which the resource-first method rules out. The map above uses
`urn:lisp:eval:runtime:{runtime}`, the eval set partitioned by the `runtime` key. Choosing
between the two spellings is [open question 1](#open-questions-for-brian). Everything below
holds for either.

| argument | required | meaning |
|---|---|---|
| `in` (or `content`) | yes | the program text. Positional or piped, like `urn:lisp:eval`. |
| `data` | no | a string the program reads. It is bound to a dynamic var (`*input*`) by a prelude the connector sends first. It is never code. |
| `ns` | no | the namespace to evaluate in. The default is `user`. |

- **Verbs:** Source and Meta. Every other verb is refused, as `urn:lisp:eval` refuses them.
- **Requires:** `urn:cap:lisp:runtime:{runtime}`, declared on the description so that the kernel
  enforces it before dispatch. Core matches a held scope against a required one exactly
  (`Capability::allows`, ikigai-core 0.3.0 `src/capability.rs`). So `urn:cap:lisp`, which
  admits the Steel sandbox, never admits a runtime that can reach the disk.
- **Faces:** `text/plain`, the last value exactly as the runtime printed it (`3`, `"typed"`,
  `nil`). An eval that sends several forms gets one `value` answer per form. Babashka was seen
  to do this ([the run](#babashkas-server-run-here)), and the connector keeps the last one.
- **Errors**, using the kernel's taxonomy:
  - `eval-error` becomes `Error::Endpoint`, whose text is the runtime's `err` text unchanged
    (ACCESSIBILITY.md: another's words are not reworded);
  - a runtime the host did not declare is `NotFound`;
  - a missing grant is `Denied`;
  - a runtime that is not running, or that does not answer within its bound, is a transient
    `Unavailable`. The connector sends `interrupt` and, if the runtime is still running after
    its grace period, kills the process;
  - an answer over the size bound is `OverBudget`.
- **Cacheability:** uncacheable, always. The runtime has state that the kernel cannot see.
- **Isolation:** each request gets a fresh session (`clone`), and the connector sends `close`
  after `done`. Nothing one request defines is visible to the next. That matches this crate's
  rule of one engine per eval.

### What the connector must handle (seen running babashka, above)

- Answers to one request arrive as several messages: `out` and `err` text, one `value` per
  top-level form, and then `done`.
- Messages about an earlier request can arrive after its `done`. Babashka's
  `wrap-track-state` sent `status ["state"]` for request 4 while request 5 was running. The
  connector matches answers by `id` and drops anything addressed to a request that has
  finished.
- `need-input` asks for `stdin`. The connector answers it once with the `data` argument, then
  with end of input, so a program never waits on a person who is not there.
- `interrupt` is real in babashka (`status ["done" "interrupted"]` on a busy loop).
  nrepl-steel's `interrupt` is a stub. Do not rely on `interrupt` alone; a bound ends in a kill.

### `urn:lisp:describe:runtime:{runtime}`

This answers the runtime's `describe` reply, with its ops and versions. The connector reads it
when a process starts, and refuses to bind a runtime whose `describe` does not list `eval`
and `clone`.

## Piece 2: the door

The door is a listener the host starts, as it starts its HTTP or QUIC door. It is not an
endpoint inside this crate's space. It turns an editor into a principal and a capability, and
then issues requests into the kernel. Its op mapping:

| op | resource | what the door does |
|---|---|---|
| `describe` | the door's own Meta (its `Description`) | lists the ops in this table as a **dictionary** (see [Op coverage](#op-coverage)), with `versions` naming `ikigai-lisp` and Steel |
| `clone`, `close`, `ls-sessions` | `urn:lisp:session:id:{id}` | makes, ends or lists the caller's own sessions. An eval without a session runs alone, as Calva's first eval does |
| `eval` | `urn:lisp:eval` | `code` becomes `in`. The answer is `value`, then `done`; an error is `err` with its own text, then `status ["eval-error"]`, then `done`. `ns` is accepted and ignored |
| `load-file` | `urn:lisp:eval` | `file` (the text, as editors send it) becomes `in`. The door never opens `file-path` |
| `interrupt` | the running eval's request | drops the eval's future. This crate already turns that drop into an interrupt the program cannot catch (README, "Stopping a runaway"). Answers `interrupted`, or `session-idle` when nothing is running |
| `stdin` | none | answers `done` and discards the text. No program can read it: the sandbox has no default input port, so no eval ever asks with `need-input` |
| `lookup`, `info`, `eldoc` | the Meta face of the name | for an IRI (`"urn:iki:fn:toUpper"`): its `Description`, with the summary as `doc` and the `ArgSpec`s as `arglists`. For a builtin (`source`, `map`): a table this crate keeps beside its allowlist |
| `completions`, `complete` | `urn:kernel:actions` and the allowlist | a prefix inside a string completes against the IRIs that `urn:kernel:actions` offers to the caller's capability. Any other prefix completes against the names a program can reach (`PROGRAM_NAMES` and `PRELUDE_NAMES`, `src/sandbox.rs`) |

**Every other op** is answered with `status ["done" "error" "unknown-op"]` and the op echoed
back. This is babashka's answer, seen in [the run](#babashkas-server-run-here). jank and nrepl-steel answer the same
way. The door never forwards an op it does not know.

**Output.** A program cannot print: `display` and the port parameters are refused names. So
the door sends no `out` or `err` except an error's text.

**The editors speak Clojure, and the door speaks Steel.** Each editor evaluates Clojure while
it connects. The sandbox answers those forms with an ordinary error (run here, see
[What the editors send](#what-the-editors-send-that-is-not-an-op)), so a handshake does not
fail. Even so, it reads poorly, and no Clojure an editor sends will do what the editor meant.
A door that fronts a remote runtime instead of `urn:lisp:eval`
([open question 3](#open-questions-for-brian)) has no such mismatch.

## Authority rules

### The client (a remote runtime)

- **C1. A remote runtime is a separate process under an OS sandbox.** A runtime's eval reaches
  files, the network and processes on its own. That was run here: through babashka's nREPL
  server, `(slurp "/etc/os-release")` read the file, and `(babashka.process/shell "true")`
  started a process (exit `0`). Nothing in nREPL stops either. So a runtime runs only as its
  own process, never in the host's. On Linux it runs in its own user, mount and network
  namespaces, with a seccomp filter. Its filesystem view is read-only and holds its own install
  plus one empty scratch directory; it has no network interface but loopback. On macOS it runs
  under a sandbox profile that grants the same. A host that cannot sandbox refuses to start
  the runtime, rather than starting it without a sandbox.
- **C2. The connector is a named exception to Hypothesis H.** Hypothesis H says an endpoint
  reaches the world only through the kernel. Today it holds by review for in-process Rust
  (`docs/formalism/README.md` in `ikigai-core`), and the ambient gate (ledger #2384) is what
  reviews it. The connector is the one module that spawns a process or opens a socket. It names
  that reach at its top, the way `ikigai-script`'s `src/backend/dir.rs` and `ikigai-intray`'s
  `src/disk.rs` name their reach to the filesystem, so a spawn or a socket anywhere else is
  still a violation the gate reports. Build it as its own crate, `ikigai-lisp-nrepl`, so this
  crate keeps no ambient reach at all.
- **C3. A runtime has its own capability.** `urn:cap:lisp:runtime:{runtime}` is declared in the
  `requires` of the runtime's eval description. `urn:cap:lisp` does not admit it, and neither does `urn:cap:lisp:run`.
  Core matches exactly, so neither scope implies the other.
- **C4. The kernel's grant never flows into a runtime.** The process gets no capability, no
  issuer, no socket to the kernel and no environment variable from the host. The connector
  starts it with an empty environment plus `HOME` set to the scratch directory. What goes in is
  program text and the `data` string. What comes out is text. A program that needs a resource
  has it sourced by the caller first and passed in as `data`, so the kernel resolves that
  resource under the caller's capability and traces it, and the runtime never reaches it. A
  runtime cannot call back into the kernel. A later design could give it callbacks, but that
  needs its own review.
- **C5. The host never exposes a runtime's port.** Babashka binds whatever address it is
  given; `bb nrepl-server 0.0.0.0:1668` listened on every interface (run here). The runtime
  listens on loopback inside its own network namespace, which nothing outside can reach, and
  the connector speaks to it through the child's stdin and stdout. Inside the sandbox, a relay
  copies the pipe to and from that loopback port. See [open question 4](#open-questions-for-brian)
  for the alternative.
- **C6. Every byte the runtime sends is bounded before it is parsed.** See
  [Bencode](#bencode-which-crate). An answer over the size bound ends the request with
  `OverBudget` and replaces the process.

### The door (an editor)

- **D1. It listens on a Unix socket or loopback only.** A Unix socket is created with mode
  `0600` in a directory owned by the service's user. A TCP address must be `127.0.0.1` or
  `::1`. Any other address is refused when the host is configured, with an error that names the
  setting and the address.
- **D2. A connection is authenticated before a message is read.** Over TCP the door speaks TLS
  and requires a client certificate. The certificate's SPKI SHA-256 fingerprint is looked up in
  `urn:lisp:grant:fingerprint:{sha256}`. A certificate with no grant is refused during the
  handshake. Over a Unix socket, the peer's uid (`SO_PEERCRED` on Linux, `getpeereid` on macOS)
  must be the service's own uid, and its grant comes from configuration. The door mints the
  grant's principal (`Capability::with_principal`, ikigai-core
  `docs/design/principal-capability.md`) and clamps the grant to the host's ceiling. The clamp
  keeps the ceiling's principal, so a peer cannot shed the identity its channel proved.

  ⚠ **The editors do not speak TLS themselves.** CIDER's client connects over TCP, over a Unix
  socket (`nrepl--unix-connect`, `nrepl-client.el`) or through SSH. No TLS option was found in
  CIDER's source as read. nREPL's own TLS (`--tls-keys-file`: a CA, the client's certificate
  and key) is mutual. An editor reaches a TLS server through a local proxy that nREPL's command
  line runs, and whose port it writes to `.nrepl-tls-proxy-port` (docs.nrepl.org,
  "Usage: server"). So on one machine the editor's path is the Unix socket. The TLS path serves
  a client on another machine, through that proxy. Whether Calva and Conjure can use a Unix
  socket was not checked ([open question 4](#open-questions-for-brian)).
- **D3. It never runs as root.** The door refuses to start when the effective uid is `0`, with
  a message saying why.
- **D4. It refuses ops it does not implement** with nREPL's `unknown-op` (above). It never
  forwards one, and never guesses at one.
- **D5. Every eval runs under the connection's capability, and the door adds none.** A door
  eval is an ordinary `urn:lisp:eval` request. It needs `urn:cap:lisp` like any other, and every
  verb inside it is checked against the same capability. A refusal reaches the editor as
  `eval-error` with the refusal's own words in `err`. When the door fronts a remote runtime
  ([open question 3](#open-questions-for-brian)), the request also needs that runtime's grant
  (C3).
- **D6. Its input is bounded before it is parsed**, as in C6. A message over a bound closes the
  connection, after the door answers with an `error` status that says which bound was exceeded.
- **D7. A session belongs to one principal.** `ls-sessions` lists only the caller's sessions.
  An op that names another principal's session is answered `unknown-session`.

## Bencode: which crate

**Recommendation: `torrust-bencode` 3.0.0, behind a pre-scan of our own.** If serde turns
out to matter, the fallback is `librqbit-bencode` 9.1.0.

The ecosystem rule (ledger #1326) is that a decoder over request input bounds nesting depth and
size *before* parsing. On a stream, there is a second reason to scan first: bencode carries no
length prefix, so a reader must find where a message ends before it can hand the message to
any decoder. Both pieces therefore read through one **pre-scan**, written in this design. It
is a single pass that does not recurse and does not allocate. It:

- checks the syntax;
- counts depth, and refuses anything deeper than its bound. The door's bound is 8: an editor's
  request nests about 3 deep. A runtime's bound is 16: babashka's answers nested 5 deep in the
  run above, in its `changed-namespaces` state messages;
- refuses a message over a byte bound. The door's is 1 MiB. A runtime's is configurable, with a
  default of 16 MiB, matching this crate's `max_input_bytes`. One of babashka's state messages
  in the run was about 30 KB;
- checks that each string's length prefix fits the bytes that remain;
- returns where the message ends.

Only that slice reaches the decoder. The crate's own depth limit then becomes a second line of
defense.

Compared on 2026-11-05 from crates.io's API and each crate's downloaded source. The source was
read, not built or run.

| crate | downloads (all / recent) | latest | license | `unsafe` | serde | depth bound | unsorted keys |
|---|---|---|---|---|---|---|---|
| `torrust-bencode` | 39,831 / 31,102 | 3.0.0, 2026-06-05 | Apache-2.0 | none | no | **yes**, `BDecodeOpt`, default 50 | accepted by default; checking is optional |
| `librqbit-bencode` | 174,295 / 104,984 | 9.1.0, 2026-10-29 | Apache-2.0 | 1, sound | yes | **yes**, `with_max_depth`, default 128 | accepted |
| `bendy` | 970,758 / 172,313 | 0.6.1, 2025-11-10 | BSD-3-Clause | 1, sound | feature | yes, default 2048, but not settable on the serde path | **always refused** |
| `serde_bencode` | 1,236,328 / 429,076 | 0.2.4, 2023-10-09 | MIT | none | yes | **none** (recursion is unbounded) | accepted |
| `bt_bencode` | 43,358 / 3,583 | 0.8.2, 2025-03-16 | MIT OR Apache-2.0 | none | yes | **none** | accepted |
| `lava_torrent` | 91,519 / 11,760 | 0.11.1, 2023-05-01 | MIT OR Apache-2.0 | none | no | **none** | refused |
| `bip_bencode` | 69,707 / 5,409 | 0.4.4, 2017-11-17 | MIT OR Apache-2.0 | none | no | yes | optional check |

Why `torrust-bencode`:

- **It bounds depth, and the bound is settable.** `src/reference/decode.rs:10` returns
  `InvalidRecursionExceeded` when `depth >= opts.max_recursion()`.
- **It accepts the dictionaries nREPL peers send.** Key order is not checked by default
  (`DEFAULT_CHECK_KEY_SORT: bool = false`, `src/reference/decode_opt.rs:2`). A duplicate key, a
  zero-padded integer or `-0` is refused, and an integer outside `i64` is an error, not a wrap.
- **It has no `unsafe`, it borrows from its input, and it has one runtime dependency**
  (`thiserror`). It is maintained in `torrust/torrust-bittorrent`. Apache-2.0 is compatible
  with this crate's `MIT OR Apache-2.0`.
- **Cost:** it has no serde. The connector reads dictionary values through the crate's access
  traits. That is a few lines for nREPL's flat messages. The crate declares
  `rust-version = "1.88"`. This crate declares no MSRV, so nothing changes in CI.

Why not the others:

- **`bendy`** refuses unsorted keys and has no switch to turn that off
  (`src/state_tracker/state.rs:80-82`). nREPL does not require canonical bencode, so a peer
  that sends keys out of order would be refused.
- **`serde_bencode`, `bt_bencode`, `lava_torrent`** recurse with no bound. A deep enough
  message overflows the stack, which aborts the whole host process. `serde_bencode` and
  `lava_torrent` have not had a release since 2023.
- **`librqbit-bencode`** is the serde fallback. Its string lengths and integers go through
  `atoi`'s `FromRadix10`, which does not check overflow (`src/deserialize.rs:84` for lengths,
  `:242-302` for integers). An oversized number is not reported as an error. That was read in
  the source, not tested. It is versioned with the rqbit application, and its depth
  bound arrived only in 2026-09 (rqbit PR #660).
- No RUSTSEC advisory exists for any of these crates (advisory-db, read 2026-11-05).
- **The existing Rust nREPL crates bound nothing.** `cljrs-nrepl` 0.2.38 and `nrepl` 0.1.1 each
  carry a recursive decoder with no depth bound. `nreplops-tool` uses `serde_bencode`. None of
  them is a codec to reuse.

## Test plan

Nothing below is built yet. Each line names what will hold the claim once it is built.

**The codec** (pure unit tests, in the shared crate):

- every message nREPL's spec shows, and every message in the babashka run above,
  round-trips byte for byte;
- with the door's bounds, the pre-scan refuses depth 9 and accepts depth 8. It refuses `l` repeated 200,000 times on a
  2 MiB thread *without* overflowing. That is the shape of the probe sent to babashka above;
- a length prefix larger than the bytes that follow it is refused before anything is
  allocated; so is a message over the byte bound, and so is `i-0e`;
- a dictionary whose keys arrive unsorted is accepted, and one with a duplicate key is refused;
- a fuzz target (`cargo fuzz`) for the pre-scan and the decoder, run by hand and not in CI.

**The client, against babashka's real server, in CI:**

- a caller job in this repository's `ci.yml` (the shared `rust.yml` owns only the cargo gates
  and says that non-cargo jobs stay in the caller). The job downloads one pinned babashka
  release, checks its published SHA-256 (as was done here for v1.13.229), and runs the
  integration tests with `IKIGAI_LISP_REQUIRE_BB=1`. Outside that job the tests skip themselves
  when `bb` is missing, and say so. Inside it they fail instead, so a missing binary cannot
  quietly turn the suite green;
- `urn:lisp:describe:runtime:bb` lists `eval`, `clone`, `close` and `interrupt`;
- `(+ 1 2)` answers `3`; `(println "x") :v` answers `:v`, which is the last value and not the
  output; `(/ 1 0)` answers an `Endpoint` error whose text is babashka's own
  `java.lang.ArithmeticException: Divide by zero` line;
- `(read-line)` with `data=typed` answers `"typed"`, and without `data` it answers `nil`. It
  never hangs;
- `(loop [] (recur))` under a 2-second bound answers `Unavailable`, and the runtime still
  answers the next request (the interrupt worked), or has been replaced (the kill worked);
- a late `status ["state"]` for a finished request is dropped, not mistaken for an answer;
- an answer over the size bound is `OverBudget`;
- **the sandbox:** `(slurp "/etc/os-release")` and `(babashka.process/shell "true")` both
  fail inside the sandbox, and an attempt to open a socket to anything but loopback fails. The
  same three forms *succeed* against a runtime started without the sandbox, in the same test,
  so the test proves the sandbox is what stopped them;
- a caller holding `urn:cap:lisp` but not `urn:cap:lisp:runtime:bb` gets `Denied` before the
  runtime is reached. The pin is that the runtime's process count does not change.

**jank and JVM Clojure** are not in CI at first. A host that declares one runs the same suite
by hand, with the runtime's name as an argument. See [open question 6](#open-questions-for-brian).

**The door, in process** (a test client built on the same codec):

- `describe` lists exactly the ops in the table above; any other op answers `unknown-op`;
- `eval` of `(+ 1 2)` answers `3`. `eval` of `(sink "urn:x" "y")` under a grant without
  that sink's scope answers `eval-error`, and `err` carries the `Denied` text unchanged;
- a connection with no certificate, or with an unmapped one, is refused during the handshake.
  The pin is that no message was read;
- a non-loopback listen address is refused when the host is configured, with an error naming
  the setting; starting as uid 0 is refused (the check is a function, tested with a fake uid);
- a session made by one principal is `unknown-session` to another;
- each refusal's text is pinned, as `tests/sandbox.rs` pins the sandbox's refusals today
  (ACCESSIBILITY.md).

**The editors, by hand at first:** connect each of CIDER, Calva and Conjure to the door and to
a door fronting babashka. Record what works in a table in the door's README. Conjure can be
driven headless from Neovim, so it is the first candidate for an automated check.

## Open questions for Brian

1. **The client's name.** The item asks for `urn:lisp:{runtime}:eval`. The resource-first
   method gives `urn:lisp:eval:runtime:{runtime}`: the type comes first, then a keyed partition.
   A third option is the `dialect=` argument that `urn:lisp:eval` already declares (only
   `steel` today). That is rejected here, because a capability can be declared per door but
   not per argument value. So `dialect=babashka` could not be refused by the kernel before
   dispatch, as C3 requires. Which spelling?
2. **Do door sessions keep definitions?** This crate isolates every eval: a `define` is gone
   by the next one. Editor users evaluate a `define` and then call it, so with isolation as it
   is, the door is a calculator. A session could keep the text of the definitions it has
   evaluated, and the door would send them ahead of each new form, as one program. That
   recompiles them every time; a corpus program of 106 KB compiled and ran in 27 ms (README).
   It also keeps isolation between sessions and needs no change to the engine. The cost is
   that a definition with an effect (a `sink`) runs again on every eval. Should the door
   replay definitions, keep each eval isolated, or wait for a design that holds an engine per
   session?
3. **Should the door also front remote runtimes?** If the door's eval target can be set to
   `urn:lisp:eval:runtime:{runtime}`, a CIDER user gets JVM Clojure or babashka with every
   Clojure feature, behind ikigai's certificate-to-grant mapping and the runtime's own grant.
   That removes the Clojure-and-Steel mismatch for those users. It also puts two grants on one
   connection (D5 and C3). Is that in scope for the first door?
4. **How does the connector reach a runtime that listens only on TCP?** Babashka and jank
   listen only on TCP. JVM nREPL can also listen on a Unix socket (`--socket`). C5 proposes a
   relay inside the sandbox that copies the child's stdin and stdout to its loopback port, so
   nothing listens outside the sandbox. The alternative, a Unix socket in the scratch
   directory, works only for JVM Clojure. The relay is one more small binary to sandbox. Which
   one? And on the door's side: can Calva and Conjure connect over a Unix socket? If they
   cannot, a local Calva user needs a TLS client certificate and nREPL's proxy.
5. **Answering the editors' handshake forms.** The door could recognize the exact forms Calva,
   Conjure and CIDER send when they connect (above) and answer them itself: `*ns*` with `user`,
   the type probe with `clj`, CIDER's init form with `nil`. Without that, each editor sees an
   error, and the error ends with this crate's isolation hint, which does not fit the case.
   This is a table of the editors' current strings, and it will drift when they change. Is that
   acceptable, or should the door pass them to the sandbox and let them fail?
6. **Which runtimes does CI hold?** Babashka is one static binary with a checksum, and the
   plan puts it in CI. jank is about 72 MB from an apt repository, and JVM Clojure needs a JDK.
   Should either have a CI job, or should a host that declares one run the suite by hand?
7. **The sandbox's mechanism.** C1 names Linux namespaces with seccomp, and a macOS sandbox
   profile. Does the ecosystem already have a jailing crate this should use? ikigai-runtime
   jails WebAssembly, not processes. If not, should that jail be a crate of its own, so that
   ikigai-browse, whose `git` runs through its jailed paths, and this connector share it?
8. **Where the door lives.** It is an edge, like `ikigai-web` and gonk's QUIC door, so it does
   not belong in this crate's space. It could go in `ikigai-lisp-nrepl` beside the connector, or
   in the host (`ikigai-cli`, gonk). Which?

## Sources

All sources were read on 2026-11-05. Commits are given where the source is a repository.

**nREPL**

- Spec draft 1.0.0: <https://spec.nrepl.org/> (repository `nrepl/spec.nrepl.org` at
  `67796e3`). It names "describe, eval, and stdin" as "the absolute minimum", requires
  `unknown-op` with `done`, and recommends `ops` as a list.
- Ops: <https://docs.nrepl.org/nrepl/ops.html>. Handlers (`done`, `unknown-op`):
  <https://docs.nrepl.org/nrepl/design/handlers.html>. Building servers:
  <https://docs.nrepl.org/nrepl/building_servers.html>. Server usage (no authentication, Unix
  sockets, TLS and its proxy): <https://docs.nrepl.org/nrepl/usage/server.html>. nREPL's latest
  release was 1.8.0, 2026-10-02.

**Runtimes**

- Babashka v1.13.229:
  <https://github.com/babashka/babashka/releases/tag/v1.13.229>, run here as described above.
  `CHANGELOG.md` and `doc/adr/ai/0003-nrepl-from-source.md` read at `df84d50f3427`.
- `babashka.nrepl` 0.0.8: <https://github.com/babashka/babashka.nrepl>, at `fb44874128d8`,
  `src/babashka/nrepl/impl/server.clj` lines 255-333.
- jank: <https://github.com/jank-lang/jank>, at `f3d82d65499920f81216e9235bd5d3a34a968e3f`,
  `compiler+runtime/src/jank/jank/nrepl/server/` and `compiler+runtime/src/cpp/jank/nrepl/server.cpp`.
  Its book: <https://book.jank-lang.org/getting-started/04-hello-nrepl.html> and
  <https://book.jank-lang.org/getting-started/01-installation.html>.
- nrepl-steel: <https://github.com/waddie/nrepl-steel>, at
  `d412c6533cadb2715b2176c5b11d26e07253b6e2`. Steel: <https://github.com/mattwparas/steel>,
  at `498eacfc41b2`.

**Editors** (source, not run)

- CIDER: <https://github.com/clojure-emacs/cider>, at `1f0a1d13f3e8`: `lisp/nrepl-client.el`,
  `lisp/cider-repl.el`, `lisp/cider-connection.el`, `lisp/cider-client.el` and
  `lisp/cider-session.el`. Also <https://docs.cider.mx/cider/platforms/other_platforms.html>.
- Calva: <https://github.com/BetterThanTomorrow/calva>, at `7886792dd1ba` (2.0.609):
  `src/nrepl/index.ts` and `src/connector.ts`. Also <https://calva.io/connect/>.
- Conjure: <https://github.com/Olical/conjure>, at `b23c76a8c874`:
  `fnl/conjure/client/clojure/nrepl/server.fnl` and `action.fnl`.

**Bencode**

- crates.io API, `https://crates.io/api/v1/crates/{name}`, and each crate's downloaded source:
  `torrust-bencode` 3.0.0 (<https://github.com/torrust/torrust-bittorrent>),
  `librqbit-bencode` 9.1.0 (<https://github.com/ikatson/rqbit>, depth bound in
  [PR #660](https://github.com/ikatson/rqbit/pull/660), merged 2026-09-08), `bendy` 0.6.1,
  `serde_bencode` 0.2.4, `bt_bencode` 0.8.2, `lava_torrent` 0.11.1, `bip_bencode` 0.4.4,
  `cljrs-nrepl` 0.2.38, `nrepl` 0.1.1 and `nreplops-tool` 0.3.1.
- RUSTSEC: <https://github.com/rustsec/advisory-db>, with no entry for any of them.

**The ecosystem**

- The resource-first method: `docs/resource-design.md` in
  <https://github.com/ikigai-rs/ikigai-gonk-starter>, at `8958273fcc4d`.
- Hypothesis H and the ambient gate (ledger #2384): `docs/formalism/README.md` and
  `docs/formal/scope-hypothesis-audit.md` in <https://github.com/ikigai-rs/ikigai-core>, at
  `d232fd7f21af`. The named exceptions are `src/backend/dir.rs` in `ikigai-script`
  (`5c77cbca19a2`) and `crates/ikigai-intray/src/disk.rs` in `ikigai-engine` (`9f7477cfcc7a`).
- Principals as capability scopes: `docs/design/principal-capability.md` in `ikigai-core`.
- Exact scope matching: `Capability::allows`, ikigai-core 0.3.0 `src/capability.rs`. `ArgSpec`:
  `src/describe.rs`. `urn:kernel:actions`: `src/kernel_ops.rs`.
- Ledger #1326, the depth bound rule, as `ikigai-rdf` and `ikigai-sparql` apply it in their
  READMEs.
- The shared CI: `.github/workflows/rust.yml` in <https://github.com/ikigai-rs/.github>, at
  `d7a47dfa16cf`. It owns the cargo gates only.
