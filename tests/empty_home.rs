//! An evaluation on an account with no `~/.local/share` is silent and writes nothing to the
//! home (ledger #3180). Steel reads its home once per process, so each case runs in a fresh
//! child process: this test binary run again with `HOME` pointed at a fixture directory.
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

const CHILD: &str = "IKIGAI_LISP_EMPTY_HOME_CHILD";

/// The child: two evaluations, each answering 42. Does nothing in an ordinary test run.
#[test]
fn child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let kernel = Kernel::new(Arc::new(ikigai_lisp::space()));
    for _ in 0..2 {
        let request = Request::new(Verb::Source, Iri::parse("urn:lisp:eval").unwrap())
            .with_arg("in", ArgRef::Inline(b"(+ 20 22)".to_vec()));
        let answer = block_on(kernel.issue(request, &Capability::root())).unwrap();
        assert_eq!(answer.bytes.as_slice(), b"42");
    }
}

/// Every path under `root`, sorted.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths.extend(tree(&path));
        }
        paths.push(path);
    }
    paths.sort();
    paths
}

/// Run [`child`] with `HOME` at `home` and no Steel or XDG settings; return its stderr.
fn run_child(home: &Path) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("HOME", home)
        .env_remove("STEEL_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "child failed: {}\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    stderr
}

#[test]
fn a_home_without_local_share_is_silent_and_untouched() {
    let home =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("empty-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let stderr = run_child(&home);
    assert_eq!(stderr, "", "an evaluation printed to stderr");
    assert_eq!(
        tree(&home),
        Vec::<PathBuf>::new(),
        "an evaluation wrote to the home"
    );

    std::fs::remove_dir_all(&home).unwrap();
}

/// The case left alone: with a data home, Steel creates its home there, as before, and is
/// silent.
#[test]
fn a_home_with_local_share_is_silent() {
    let home =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("data-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".local/share")).unwrap();

    assert_eq!(run_child(&home), "", "an evaluation printed to stderr");

    std::fs::remove_dir_all(&home).unwrap();
}
