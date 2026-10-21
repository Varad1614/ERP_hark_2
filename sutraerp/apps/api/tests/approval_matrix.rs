//! Part 1 · Stage 3 (S3-appr) — the approval control matrix as a fitness test.
//!
//! Enumerates **every** decision endpoint of the API — POST operations whose path ends in
//! `…/decision`, `…-decision`, `/authorise`, `/approve`, `/approve-…`, `/approval` or `/reject` —
//! from both `openapi/openapi.json` **and the router source** (every `#[utoipa::path(post, …)]`
//! and macro-generated route in `modules/`, so a new decision endpoint is caught before the
//! OpenAPI document is regenerated; audit C1) and fails when one
//! is neither
//!
//! * **guarded** — its handler (or a function it calls in the same module, one level deep, or the
//!   macro that generates it) calls the separation-of-duties guard
//!   (`sutra_persistence::approval::guard_decision*` or a module's `ensure_independent_decider`),
//!   nor
//! * on the documented [`ALLOWLIST`] with the reason a self-decision is acceptable.
//!
//! It also fails when an allowlisted operation is guarded or gone (stale allowlist) and when
//! `docs/readiness/APPROVAL_CONTROL_MATRIX.md` does not list an operation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Decisions where a self-decision is accepted, and why (mirrored in the matrix document).
const ALLOWLIST: &[(&str, &str)] = &[
    ("offering_decision", "academic catalogue approval of an offering (dates, capacity, sections); no money, grade or student-status change; seats still need a registration or a guarded override"),
    ("offering_simulation_decision", "not a checker decision: the planner proceeds / revises / rejects their own change-impact simulation; proceeding requests the governed approval"),
    ("timetable_decision", "scheduling of teaching slots; no money, grade or status change"),
    ("learning_provider_decision", "catalogue onboarding of an external provider; credits only flow through the guarded flexible-enrolment / credit-recognition decisions"),
    ("learning_opportunity_decision", "catalogue activation of a learning opportunity; credits only flow through guarded decisions"),
    ("campus_infra_node_decommission_decide", "facilities operation (space decommissioning); no money, grade, status or access change"),
    ("campus_maintenance_plan_decide", "facilities maintenance planning; spend is procured through guarded procurement decisions"),
    ("campus_reservation_decide", "room / space booking of a facility slot; no money, grade, status or access change (fees, if any, go through guarded finance decisions)"),
    ("campus_transport_route_decide", "transport route activation; fees go through guarded finance decisions"),
    ("exam_publication_authorise", "the maker's request step of result publication; the decision is exam_publication_decide (guarded)"),
    ("exam_malpractice_decide", "the maker's proposal step of a malpractice committee decision (it requests MALPRACTICE_DECISION); the governed decision is exam_malpractice_approval (guarded) — DEF-P2-PILOT-001"),
    ("exam_supplementary_authorise", "single-step authorisation of a rule-computed supplementary attempt (no maker request); marks and results of the attempt pass the guarded mark-sheet / result approvals"),
    ("resources_stock_issue_decide", "internal issue of stocked consumables to a department (inventory.manage); no supplier payment; stock value changes by adjustment are guarded"),
    ("institution_decide_activation", "institution set-up by the implementation administrator; no person's money, grades or status"),
    ("campus_decision", "institution set-up: campus activation / decommissioning"),
    ("org_type_blueprint_decide", "institution set-up: organisation type blueprint"),
    ("academic_structure_decision", "institution set-up: academic structure"),
    ("school_model_decision", "institution set-up: school model"),
    ("curriculum_version_decision", "institution set-up: curriculum version publication; grades follow the guarded exam decisions"),
    ("academic_year_decision", "institution set-up: academic year closure"),
    ("calendar_version_decision", "institution set-up: calendar publication"),
    ("calendar_exception_decision", "institution set-up: calendar exception"),
    ("calendar_simulation_decision", "institution set-up: calendar change simulation"),
    ("services_accreditation_cycle_decide", "reporting configuration: accreditation plan"),
    ("services_accreditation_framework_decide", "reporting configuration: accreditation framework"),
    ("services_dashboard_decide", "reporting configuration: dashboard publication (read-only analytics)"),
    ("services_regulatory_framework_decide", "reporting configuration: regulatory framework"),
    // Audit C1 (2026-10): `/reject` and `/approve-…` joined the enumeration.
    ("people_recruitment_candidate_reject", "recruitment pipeline step (screening out a candidate); grants nothing — the hire goes through the guarded offer approval"),
    ("resources_receipt_reject", "goods-receipt inspection rejecting a supplier delivery; grants nothing — supplier payment only follows the guarded invoice / payment approvals"),
    ("services_regulatory_snapshot_reject", "sends a regulatory report snapshot back; grants nothing (a self-rejection equals withdrawing it)"),
    ("admission_application_reject", "screening rejection of an applicant's application before any admission; grants nothing — admission itself is the guarded admission decision"),
    ("document_request_reject", "refuses a student's document request; grants nothing (no money, grade or status change)"),
];

const GUARDS: &[&str] = &["guard_decision", "ensure_independent_decider"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Whether the last path segment names a checker decision.
fn is_decision_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    // Part 2 · S6-pilot (DEF-P2-PILOT-001): `…/approval` too — the malpractice approval
    // (`exam_malpractice_approval`) escaped this enumeration and had no guard.
    // Audit C1: `/approve-…` (e.g. revaluation `approve-marks`) and `/reject` too.
    last == "decision"
        || last.ends_with("-decision")
        || last == "authorise"
        || last == "approve"
        || last.starts_with("approve-")
        || last == "approval"
        || last == "reject"
}

/// Decision operations of the committed OpenAPI document: operationId → path.
fn openapi_decision_operations() -> BTreeMap<String, String> {
    let spec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root().join("openapi/openapi.json")).unwrap())
            .unwrap();
    let mut ops = BTreeMap::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        let Some(post) = item.get("post") else {
            continue;
        };
        if is_decision_path(path) {
            ops.insert(
                post["operationId"].as_str().unwrap().to_owned(),
                path.clone(),
            );
        }
    }
    ops
}

/// The string literal starting at `s[at] == '"'`, and the index after its closing quote.
fn literal(s: &str, at: usize) -> Option<(&str, usize)> {
    let rest = s.get(at..)?.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((&rest[..end], at + 1 + end + 1))
}

/// Audit C1: decision operations declared in the router source of `modules/` — every
/// `#[utoipa::path(post, path = "…", operation_id = "…")]` handler and every macro-generated
/// route `name!(handler, "/api/v1/…", "operation_id", …)` — operationId → path.
fn source_decision_operations(files: &[(PathBuf, String)]) -> BTreeMap<String, String> {
    let mut ops = BTreeMap::new();
    for (_, text) in files {
        let mut from = 0;
        while let Some(i) = text[from..].find("#[utoipa::path(") {
            let at = from + i + "#[utoipa::path".len();
            let Some((b0, b1)) = block(text, at, '(', ')') else {
                break;
            };
            let attr = &text[b0 + 1..b1 - 1];
            from = b1;
            if !attr.trim_start().starts_with("post") {
                continue;
            }
            let field = |name: &str| {
                let k = attr.find(&format!("{name} = "))? + name.len() + 3;
                literal(attr, k).map(|(v, _)| v.to_owned())
            };
            if let (Some(path), Some(op)) = (field("path"), field("operation_id")) {
                if is_decision_path(&path) {
                    ops.insert(op, path);
                }
            }
        }
        // macro-generated routes: a path literal directly followed by an operation id literal
        for (i, _) in text.match_indices("\"/api/v1/") {
            let Some((path, after)) = literal(text, i) else {
                continue;
            };
            let rest = &text[after..];
            let Some(r) = rest.strip_prefix(',') else {
                continue;
            };
            let skipped = after + 1 + (r.len() - r.trim_start().len());
            if let Some((op, _)) = literal(text, skipped) {
                if is_decision_path(path) && !op.is_empty() && !op.contains(' ') {
                    ops.insert(op.to_owned(), path.to_owned());
                }
            }
        }
    }
    ops
}

fn module_files() -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    rust_files(&root().join("modules"), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let t = fs::read_to_string(&p).unwrap();
            (p, t)
        })
        .collect()
}

/// Decision operations: the committed OpenAPI document ∪ the router source.
fn decision_operations() -> BTreeMap<String, String> {
    let mut ops = openapi_decision_operations();
    ops.extend(source_decision_operations(&module_files()));
    ops
}

/// The balanced `{…}` / `(…)` block starting at the first `open` at or after `from`.
fn block(s: &str, from: usize, open: char, close: char) -> Option<(usize, usize)> {
    let start = from + s[from..].find(open)?;
    let mut depth = 0usize;
    for (i, c) in s[start..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((start, start + i + 1));
            }
        }
    }
    None
}

fn has_guard(text: &str) -> bool {
    GUARDS.iter().any(|g| text.contains(g))
}

/// Body of `fn name` in `text`, if defined there.
fn fn_body<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = text[from..].find(&format!("fn {name}")) {
        let at = from + i + 3 + name.len();
        let next = text[at..].chars().next();
        if matches!(next, Some('(') | Some('<')) {
            let (b0, b1) = block(text, at, '{', '}')?;
            return Some(&text[b0..b1]);
        }
        from = at;
    }
    None
}

/// `(qualifier, name)` of every call `q::name(` / `name(` in `text`.
fn calls(text: &str) -> Vec<(Option<String>, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    for (i, _) in text.match_indices('(') {
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_alphanumeric() || b[j - 1] == b'_') {
            j -= 1;
        }
        if j == i {
            continue;
        }
        let name = text[j..i].to_owned();
        let mut q = None;
        if j >= 2 && &text[j - 2..j] == "::" {
            let mut k = j - 2;
            while k > 0 && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_') {
                k -= 1;
            }
            q = Some(text[k..j - 2].to_owned());
        }
        out.push((q, name));
    }
    out
}

/// The handler text of `op` and the file it is in: the function after `operation_id = "op"`, or
/// the macro invocation carrying `"op"` (plus the macro's definition in the same file).
fn handler(files: &[(PathBuf, String)], op: &str) -> Option<(PathBuf, String)> {
    let direct = format!("operation_id = \"{op}\"");
    for (p, text) in files {
        if let Some(i) = text.find(&direct) {
            let f = i + text[i..].find("async fn ")?;
            let (b0, b1) = block(text, f, '{', '}')?;
            return Some((p.clone(), text[b0..b1].to_owned()));
        }
    }
    // macro-generated handlers: the `name!(…)` invocation carrying `"op",`
    let lit = format!("\"{op}\",");
    for (p, text) in files {
        let Some(i) = text.find(&lit) else {
            continue;
        };
        let head = &text[..i];
        let Some(bang) = head.rfind("!(") else {
            continue;
        };
        let b = head.as_bytes();
        let mut k = bang;
        while k > 0 && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_') {
            k -= 1;
        }
        let mac = &head[k..bang];
        let def = format!("macro_rules! {mac}");
        let Some(d) = text.find(&def) else {
            continue;
        };
        let (b0, b1) = block(text, bang, '(', ')')?;
        let mut out = text[b0..b1].to_owned();
        if let Some((m0, m1)) = block(text, d, '{', '}') {
            out.push_str(&text[m0..m1]);
        }
        return Some((p.clone(), out));
    }
    None
}

/// Guarded: the handler text, or a function it calls (same directory first, then the module
/// crate, matching the call's qualifier to the file stem / directory), contains the guard.
fn guarded(files: &[(PathBuf, String)], file: &Path, text: &str) -> bool {
    if has_guard(text) {
        return true;
    }
    let crate_src = file
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "src"))
        .unwrap()
        .to_path_buf();
    let dir = file.parent().unwrap().to_path_buf();
    for (q, name) in calls(text) {
        let candidates: Vec<&(PathBuf, String)> = files
            .iter()
            .filter(|(p, _)| p.starts_with(&crate_src))
            .filter(|(p, _)| match &q {
                Some(q) if q != "crate" && q != "super" && q != "self" => {
                    p.file_stem().is_some_and(|s| s == q.as_str())
                        || p.parent()
                            .and_then(|d| d.file_name())
                            .is_some_and(|d| d == q.as_str())
                }
                _ => true,
            })
            .collect();
        let same_dir: Vec<_> = candidates
            .iter()
            .filter(|(p, _)| p.parent() == Some(dir.as_path()))
            .collect();
        let pool: Vec<&&(PathBuf, String)> = if same_dir.is_empty() {
            candidates.iter().collect()
        } else {
            same_dir.into_iter().collect()
        };
        if pool
            .iter()
            .filter_map(|(_, t)| fn_body(t, &name))
            .any(has_guard)
        {
            return true;
        }
    }
    false
}

#[test]
fn every_decision_endpoint_is_guarded_or_documented() {
    let ops = decision_operations();
    assert!(
        ops.len() >= 120,
        "expected the full set of decision endpoints, found {}",
        ops.len()
    );
    let files = module_files();
    let allow: BTreeMap<&str, &str> = ALLOWLIST.iter().copied().collect();
    let mut failures = Vec::new();
    for (op, path) in &ops {
        let Some((file, text)) = handler(&files, op) else {
            failures.push(format!("{op} ({path}): handler not found in modules/"));
            continue;
        };
        let g = guarded(&files, &file, &text);
        match (g, allow.get(op.as_str())) {
            (true, None) | (false, Some(_)) => {}
            (true, Some(_)) => failures.push(format!(
                "{op} ({path}) is guarded: remove it from the allowlist"
            )),
            (false, None) => failures.push(format!(
                "{op} ({path}) in {} has no separation-of-duties guard and is not allowlisted",
                file.display()
            )),
        }
    }
    for (op, _) in ALLOWLIST {
        if !ops.contains_key(*op) {
            failures.push(format!(
                "allowlisted {op} is not a decision endpoint any more"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_approval_control_matrix_lists_every_decision_endpoint() {
    let doc = fs::read_to_string(root().join("docs/readiness/APPROVAL_CONTROL_MATRIX.md")).unwrap();
    let missing: Vec<String> = decision_operations()
        .into_keys()
        .filter(|op| !doc.contains(&format!("`{op}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "APPROVAL_CONTROL_MATRIX.md misses: {}",
        missing.join(", ")
    );
}

#[test]
fn allowlist_reasons_are_documented() {
    for (op, why) in ALLOWLIST {
        assert!(
            why.len() > 20,
            "{op}: give the reason a self-decision is acceptable"
        );
    }
}

/// Audit C1: the router scan sees every decision endpoint the OpenAPI document has (so the
/// source scan is trustworthy for endpoints not yet in the document).
#[test]
fn the_router_scan_finds_every_documented_decision_endpoint() {
    let source = source_decision_operations(&module_files());
    let missed: Vec<String> = openapi_decision_operations()
        .into_iter()
        .filter(|(op, _)| !source.contains_key(op))
        .map(|(op, path)| format!("{op} ({path})"))
        .collect();
    assert!(
        missed.is_empty(),
        "the router scan missed: {}",
        missed.join(", ")
    );
}
