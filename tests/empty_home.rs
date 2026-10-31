//! Engine initialization must be quiet and leave the host's home untouched.
use std::{path::Path, process::Command, sync::Arc};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

const CHILD: &str = "IKIGAI_LISP_EMPTY_HOME_CHILD";

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
    assert_eq!(steel::compiler::modules::steel_home(), None);
}

fn tree(root: &Path) -> Vec<std::path::PathBuf> {
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

#[test]
fn evaluations_need_no_home_directory() {
    // Keep fixture writes in this checkout; each child starts with fresh Steel globals.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("empty-home-{}", std::process::id()));
    for scenario in [
        "empty",
        "data-home",
        "legacy-home",
        "steel-home",
        "xdg-home",
    ] {
        let home = root.join(scenario);
        std::fs::create_dir_all(&home).unwrap();
        match scenario {
            "data-home" => std::fs::create_dir_all(home.join(".local/share")).unwrap(),
            "legacy-home" => std::fs::create_dir(home.join(".steel")).unwrap(),
            "steel-home" | "xdg-home" => std::fs::create_dir(home.join("configured")).unwrap(),
            _ => {}
        }
        let before = tree(&home);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "child", "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("STEEL_HOME")
            .env_remove("XDG_DATA_HOME");
        match scenario {
            "steel-home" => {
                command.env("STEEL_HOME", home.join("configured"));
            }
            "xdg-home" => {
                command.env("XDG_DATA_HOME", home.join("configured"));
            }
            _ => {}
        }
        let output = command.output().unwrap();
        let after = tree(&home);
        std::fs::remove_dir_all(&home).unwrap();
        assert!(
            output.status.success(),
            "{scenario}: child failed: {output:?}"
        );
        assert!(
            output.stderr.is_empty(),
            "{scenario}: engine wrote to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(before, after, "{scenario}: engine wrote to HOME");
    }
    std::fs::remove_dir(&root).unwrap();
}
