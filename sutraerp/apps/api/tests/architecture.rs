//! Architecture fitness tests (ADR-L5-0003 / ADR-L5-0036): enforce module boundaries at the
//! dependency-graph level so a violation fails `cargo test`, not code review.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Internal (`sutra-*`) dependencies declared in a manifest's `[dependencies]` table.
fn internal_deps(manifest: &Path) -> Vec<String> {
    let text = fs::read_to_string(manifest).unwrap();
    let mut in_deps = false;
    let mut out = vec![];
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_deps = l == "[dependencies]";
            continue;
        }
        if in_deps && l.starts_with("sutra-") {
            out.push(l.split(['.', ' ', '=']).next().unwrap().to_owned());
        }
    }
    out
}

fn module_dirs() -> Vec<PathBuf> {
    let mut v: Vec<_> = fs::read_dir(root().join("modules"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("Cargo.toml").exists())
        .collect();
    v.sort();
    v
}

#[test]
fn modules_depend_only_on_shared_crates() {
    let allowed = [
        "sutra-kernel",
        "sutra-contracts",
        "sutra-web",
        "sutra-persistence",
    ];
    let dirs = module_dirs();
    assert_eq!(dirs.len(), 10, "expected the ten domain modules A–J");
    for dir in dirs {
        for dep in internal_deps(&dir.join("Cargo.toml")) {
            assert!(
                allowed.contains(&dep.as_str()),
                "{} depends on {dep}: modules may only talk through sutra-contracts",
                dir.display()
            );
        }
    }
}

#[test]
fn shared_crates_never_depend_on_modules() {
    for (krate, allowed) in [
        ("kernel", &[][..]),
        ("contracts", &["sutra-kernel"][..]),
        ("web", &["sutra-kernel"][..]),
        ("persistence", &["sutra-kernel"][..]),
        ("infra", &["sutra-kernel"][..]),
    ] {
        for dep in internal_deps(&root().join("crates").join(krate).join("Cargo.toml")) {
            assert!(
                allowed.contains(&dep.as_str()),
                "crates/{krate} must not depend on {dep}"
            );
        }
    }
}

#[test]
fn kernel_and_contracts_are_framework_free() {
    for krate in ["kernel", "contracts"] {
        let text =
            fs::read_to_string(root().join("crates").join(krate).join("Cargo.toml")).unwrap();
        for forbidden in ["axum", "sqlx", "tower"] {
            assert!(
                !text.contains(&format!("\n{forbidden}")),
                "crates/{krate} must not depend on {forbidden} (ADR-L5-0019 §7)"
            );
        }
    }
}

/// Product SDKs (NATS, Valkey/Redis, SMTP) live only in `sutra-infra` behind kernel ports
/// (ADR-L5-0022 / ADR-L5-0037): domain modules and the other shared crates never see them.
#[test]
fn broker_and_cache_sdks_stay_in_infra() {
    let mut manifests: Vec<PathBuf> = module_dirs().iter().map(|d| d.join("Cargo.toml")).collect();
    for krate in ["kernel", "contracts", "web", "persistence"] {
        manifests.push(root().join("crates").join(krate).join("Cargo.toml"));
    }
    for m in manifests {
        let text = fs::read_to_string(&m).unwrap();
        for sdk in ["async-nats", "redis", "fred", "lettre"] {
            assert!(
                !text.contains(&format!("\n{sdk}")),
                "{} depends on {sdk}; use the kernel port and the sutra-infra adapter",
                m.display()
            );
        }
    }
}

#[test]
fn modules_do_not_touch_foreign_schemas() {
    let schemas = [
        "platform",
        "institution",
        "student",
        "academic",
        "people",
        "finance",
        "campus",
        "services",
        "exams",
        "resources",
    ];
    for dir in module_dirs() {
        let own = dir.file_name().unwrap().to_str().unwrap().to_owned();
        let mut stack = vec![dir.join("src")];
        while let Some(p) = stack.pop() {
            for e in fs::read_dir(&p).unwrap() {
                let path = e.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let src = fs::read_to_string(&path).unwrap();
                for s in schemas.iter().filter(|s| **s != own) {
                    for verb in ["FROM", "INTO", "UPDATE", "JOIN"] {
                        let needle = format!("{verb} {s}.");
                        assert!(
                            !src.contains(&needle),
                            "{} references foreign schema via `{needle}`",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_module_is_composed() {
    let names: Vec<_> = sutra_api::app::modules().iter().map(|m| m.key).collect();
    for dir in module_dirs() {
        let key = dir.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(
            names.contains(&key.as_str()),
            "module {key} is not registered in the composition root"
        );
    }
    let mut domains: Vec<_> = sutra_api::app::modules().iter().map(|m| m.domain).collect();
    domains.dedup();
    assert_eq!(domains, ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"]);
}
