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

/// Batch 4 (Domain D): every `/api/v1/academic` operation carries an explicit `operationId` with
/// its slice's prefix and the matching tag (`docs/batch4-contracts.md` §5.2), so the three
/// slices never collide in the generated TS client.
#[tokio::test]
async fn academic_operations_follow_the_slice_prefix_and_tag_rules() {
    const RULES: &[(&str, &[&str])] = &[
        // 4A
        (
            "offerings",
            &[
                "offering_",
                "offering_section_",
                "section_enrolment_",
                "offering_waitlist_",
                "offering_window_",
                "offering_quota_",
                "offering_template_",
                "offering_simulation_",
                "offering_self_",
            ],
        ),
        (
            "faculty-assignments",
            &["faculty_assignment_", "faculty_policy_", "faculty_self_"],
        ),
        // 4B
        (
            "timetable",
            &["timetable_", "scheduling_requirement_", "timetable_self_"],
        ),
        (
            "attendance",
            &["teaching_session_", "attendance_", "syllabus_coverage_"],
        ),
        ("monitoring", &["monitoring_"]),
        (
            "assessment",
            &["assessment_", "internal_assessment_", "assessment_self_"],
        ),
        // 4C
        (
            "flexible-learning",
            &[
                "learning_provider_",
                "learning_opportunity_",
                "flexible_enrolment_",
                "flexible_policy_",
                "flexible_exception_",
                "flexible_self_",
            ],
        ),
        (
            "credit-recognition",
            &[
                "certificate_verification_",
                "credit_mapping_",
                "credit_recognition_",
                "reconciliation_",
            ],
        ),
        ("resources", &["course_resource_", "resource_self_"]),
    ];
    let spec: serde_json::Value =
        serde_json::from_str(&sutra_api::openapi::spec_json().unwrap()).unwrap();
    for (path, item) in spec["paths"].as_object().unwrap() {
        let academic = path.starts_with("/api/v1/academic/");
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let academic_tag = tags.iter().any(|t| RULES.iter().any(|(tag, _)| tag == t));
            assert_eq!(
                academic, academic_tag,
                "{method} {path}: Domain D tags are used exactly for /api/v1/academic paths"
            );
            if !academic {
                continue;
            }
            let id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            let ok = RULES.iter().any(|(tag, prefixes)| {
                tags.contains(tag) && prefixes.iter().any(|p| id.starts_with(p))
            });
            assert!(
                ok,
                "{method} {path}: operationId `{id}` does not match the prefixes of its tag {tags:?}"
            );
        }
    }
}

/// Batch 5 (Domain F): every `/api/v1/finance` operation carries an explicit `operationId` with
/// its slice's prefix and the matching tag (`docs/batch5-contracts.md` §5.2); finance tags are
/// used exactly for finance paths, and no other domain uses a `finance_` operation id.
#[tokio::test]
async fn finance_operations_follow_the_slice_prefix_and_tag_rules() {
    const RULES: &[(&str, &[&str])] = &[
        // foundation
        ("finance-accounts", &["finance_account_"]),
        // 5A
        (
            "finance-charges",
            &[
                "finance_fee_head_",
                "finance_fee_structure_",
                "finance_charge_",
                "finance_instalment_",
                "finance_dispute_",
            ],
        ),
        ("finance-adjustments", &["finance_adjustment_"]),
        ("financial-aid", &["finance_aid_"]),
        ("finance-config", &["finance_config_", "finance_rule_"]),
        // 5B
        (
            "finance-payments",
            &[
                "finance_payment_",
                "finance_receipt_",
                "finance_unidentified_",
                "finance_collection_",
                "finance_gateway_",
            ],
        ),
        ("finance-refunds", &["finance_refund_"]),
        (
            "finance-receivables",
            &["finance_receivable_", "finance_dunning_"],
        ),
        // 5C
        (
            "finance-ledger",
            &[
                "finance_ledger_",
                "finance_journal_",
                "finance_batch_",
                "finance_reconciliation_",
                "finance_trial_balance_",
            ],
        ),
        ("finance-periods", &["finance_year_", "finance_period_"]),
        (
            "finance-treasury",
            &[
                "finance_bank_",
                "finance_statement_",
                "finance_transfer_",
                "finance_cash_",
            ],
        ),
    ];
    let spec: serde_json::Value =
        serde_json::from_str(&sutra_api::openapi::spec_json().unwrap()).unwrap();
    let mut finance_ops = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        let finance = path.starts_with("/api/v1/finance/");
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let finance_tag = tags.iter().any(|t| RULES.iter().any(|(tag, _)| tag == t));
            assert_eq!(
                finance, finance_tag,
                "{method} {path}: Domain F tags are used exactly for /api/v1/finance paths"
            );
            let id = op["operationId"].as_str().unwrap_or_default();
            if !finance {
                assert!(
                    !id.starts_with("finance_"),
                    "{method} {path}: `finance_` operation ids are reserved for Domain F"
                );
                continue;
            }
            finance_ops += 1;
            assert!(!id.is_empty(), "{method} {path} has no operationId");
            let ok = RULES.iter().any(|(tag, prefixes)| {
                tags.contains(tag) && prefixes.iter().any(|p| id.starts_with(p))
            });
            assert!(
                ok,
                "{method} {path}: operationId `{id}` does not match the prefixes of its tag {tags:?}"
            );
            // Path parameter rule (§5.2): the first parameter after a collection is `{id}`.
            if let Some(first) = path.split('/').find(|seg| seg.starts_with('{')) {
                assert_eq!(
                    first, "{id}",
                    "{method} {path}: first path parameter must be {{id}}"
                );
            }
        }
    }
    assert!(
        finance_ops >= 3,
        "the foundation account endpoints are mounted"
    );
}

/// Batch 6 (Domain I): every `/api/v1/exams` operation carries an explicit `operationId` with its
/// slice's prefix and the matching tag (`docs/batch6-contracts.md` §5.2); exam tags are used
/// exactly for exams paths, no other domain uses an `exam_` operation id, and the first path
/// parameter after a collection is `{id}`.
#[tokio::test]
async fn exam_operations_follow_the_slice_prefix_and_tag_rules() {
    const RULES: &[(&str, &[&str])] = &[
        // foundation
        ("exam-grading", &["exam_grading_"]),
        // 6A
        (
            "exam-setup",
            &["exam_policy_", "exam_session_", "exam_paper_"],
        ),
        (
            "exam-scheduling",
            &[
                "exam_schedule_",
                "exam_sitting_",
                "exam_venue_",
                "exam_seating_",
                "exam_invigilation_",
                "exam_conduct_",
                "exam_exception_",
            ],
        ),
        (
            "exam-registration",
            &["exam_registration_", "exam_hall_ticket_"],
        ),
        // 6B
        (
            "exam-marks",
            &[
                "exam_mark_sheet_",
                "exam_marks_",
                "exam_evaluation_",
                "exam_moderation_",
                "exam_gradebook_",
            ],
        ),
        ("exam-integrity", &["exam_malpractice_"]),
        // 6C
        (
            "exam-results",
            &["exam_result_", "exam_publication_", "exam_grade_card_"],
        ),
        (
            "exam-revaluation",
            &["exam_revaluation_", "exam_supplementary_"],
        ),
        ("exam-completion", &["exam_completion_", "exam_closure_"]),
    ];
    let spec: serde_json::Value =
        serde_json::from_str(&sutra_api::openapi::spec_json().unwrap()).unwrap();
    let mut exam_ops = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        let exams = path.starts_with("/api/v1/exams/");
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let exam_tag = tags.iter().any(|t| RULES.iter().any(|(tag, _)| tag == t));
            assert_eq!(
                exams, exam_tag,
                "{method} {path}: Domain I tags are used exactly for /api/v1/exams paths"
            );
            let id = op["operationId"].as_str().unwrap_or_default();
            if !exams {
                assert!(
                    !id.starts_with("exam_"),
                    "{method} {path}: `exam_` operation ids are reserved for Domain I"
                );
                continue;
            }
            exam_ops += 1;
            assert!(!id.is_empty(), "{method} {path} has no operationId");
            let ok = RULES.iter().any(|(tag, prefixes)| {
                tags.contains(tag) && prefixes.iter().any(|p| id.starts_with(p))
            });
            assert!(
                ok,
                "{method} {path}: operationId `{id}` does not match the prefixes of its tag {tags:?}"
            );
            if let Some(first) = path.split('/').find(|seg| seg.starts_with('{')) {
                assert_eq!(
                    first, "{id}",
                    "{method} {path}: first path parameter must be {{id}}"
                );
            }
        }
    }
    assert!(
        exam_ops >= 4,
        "the foundation grading-scheme endpoints are mounted"
    );
}

/// Batch 7 (Domain E): every `/api/v1/people` operation carries an explicit `operationId` with its
/// slice's prefix and the matching tag (`docs/batch7-contracts.md` §5.2); people tags are used
/// exactly for people paths, no other domain uses a `people_` operation id, and the first path
/// parameter after a collection is `{id}`.
#[tokio::test]
async fn people_operations_follow_the_slice_prefix_and_tag_rules() {
    const RULES: &[(&str, &[&str])] = &[
        // foundation
        ("people-directory", &["people_directory_"]),
        // 7A
        (
            "people-workforce",
            &[
                "people_staff_",
                "people_engagement_",
                "people_position_",
                "people_reporting_",
                "people_faculty_",
                "people_profile_",
            ],
        ),
        (
            "people-hr",
            &[
                "people_recruitment_",
                "people_onboarding_",
                "people_probation_",
                "people_separation_",
                "people_certificate_",
            ],
        ),
        // 7B
        ("people-leave", &["people_leave_"]),
        (
            "people-attendance",
            &["people_attendance_", "people_workload_"],
        ),
        // 7C
        (
            "people-payroll",
            &[
                "people_payroll_",
                "people_pay_",
                "people_salary_",
                "people_payslip_",
                "people_settlement_",
            ],
        ),
    ];
    let spec: serde_json::Value =
        serde_json::from_str(&sutra_api::openapi::spec_json().unwrap()).unwrap();
    let mut people_ops = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        let people = path.starts_with("/api/v1/people/");
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let people_tag = tags.iter().any(|t| RULES.iter().any(|(tag, _)| tag == t));
            assert_eq!(
                people, people_tag,
                "{method} {path}: Domain E tags are used exactly for /api/v1/people paths"
            );
            let id = op["operationId"].as_str().unwrap_or_default();
            if !people {
                assert!(
                    !id.starts_with("people_"),
                    "{method} {path}: `people_` operation ids are reserved for Domain E"
                );
                continue;
            }
            people_ops += 1;
            assert!(!id.is_empty(), "{method} {path} has no operationId");
            let ok = RULES.iter().any(|(tag, prefixes)| {
                tags.contains(tag) && prefixes.iter().any(|p| id.starts_with(p))
            });
            assert!(
                ok,
                "{method} {path}: operationId `{id}` does not match the prefixes of its tag {tags:?}"
            );
            if let Some(first) = path.split('/').find(|seg| seg.starts_with('{')) {
                assert_eq!(
                    first, "{id}",
                    "{method} {path}: first path parameter must be {{id}}"
                );
            }
        }
    }
    assert!(
        people_ops >= 4,
        "the foundation staff-directory endpoints are mounted"
    );
}

/// Batch 8 (Domains G, H, J): every `/api/v1/campus`, `/api/v1/services` and `/api/v1/resources`
/// operation carries an explicit `operationId` with its slice's prefix and the matching tag
/// (`docs/batch8-contracts.md` §5.2); Batch 8 tags are used exactly for those paths, no other
/// path uses a Batch 8 operation-id prefix (Domain B's `campus_*` campus ids keep their bare
/// prefix), and the first path parameter after a collection is `{id}`.
#[tokio::test]
async fn batch8_operations_follow_the_slice_prefix_and_tag_rules() {
    const RULES: &[(&str, &str, &[&str])] = &[
        // foundation
        (
            "/api/v1/campus/",
            "campus-directory",
            &["campus_directory_"],
        ),
        (
            "/api/v1/services/",
            "services-directory",
            &["services_directory_"],
        ),
        (
            "/api/v1/resources/",
            "resources-directory",
            &["resources_directory_"],
        ),
        // 8A
        (
            "/api/v1/campus/",
            "campus-infrastructure",
            &["campus_infra_", "campus_space_", "campus_reservation_"],
        ),
        (
            "/api/v1/campus/",
            "campus-facilities",
            &[
                "campus_ticket_",
                "campus_workorder_",
                "campus_maintenance_",
                "campus_lab_",
            ],
        ),
        // 8B
        ("/api/v1/campus/", "campus-hostel", &["campus_hostel_"]),
        (
            "/api/v1/campus/",
            "campus-transport",
            &["campus_transport_"],
        ),
        // 8C
        ("/api/v1/campus/", "campus-library", &["campus_library_"]),
        (
            "/api/v1/services/",
            "services-dashboards",
            &["services_dashboard_"],
        ),
        (
            "/api/v1/services/",
            "services-regulatory",
            &["services_regulatory_"],
        ),
        // 8D
        (
            "/api/v1/resources/",
            "resources-procurement",
            &[
                "resources_supplier_",
                "resources_requisition_",
                "resources_sourcing_",
                "resources_po_",
                "resources_receipt_",
                "resources_payable_",
            ],
        ),
        (
            "/api/v1/resources/",
            "resources-inventory",
            &["resources_item_", "resources_store_", "resources_stock_"],
        ),
        (
            "/api/v1/resources/",
            "resources-assets",
            &["resources_asset_"],
        ),
        // Batch 9 · 9E carry-overs (docs/batch9-contracts.md §6.3)
        (
            "/api/v1/campus/",
            "campus-governance",
            &["campus_governance_"],
        ),
        (
            "/api/v1/services/",
            "services-accreditation",
            &["services_accreditation_"],
        ),
    ];
    let spec: serde_json::Value =
        serde_json::from_str(&sutra_api::openapi::spec_json().unwrap()).unwrap();
    let all_prefixes: Vec<&str> = RULES
        .iter()
        .flat_map(|(_, _, p)| p.iter().copied())
        .collect();
    let mut ops = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        let batch8 = ["/api/v1/campus/", "/api/v1/services/", "/api/v1/resources/"]
            .iter()
            .any(|p| path.starts_with(p));
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            let batch8_tag = tags
                .iter()
                .any(|t| RULES.iter().any(|(_, tag, _)| tag == t));
            assert_eq!(
                batch8, batch8_tag,
                "{method} {path}: Batch 8 tags are used exactly for campus / services / resources paths"
            );
            let id = op["operationId"].as_str().unwrap_or_default();
            if !batch8 {
                assert!(
                    !all_prefixes.iter().any(|p| id.starts_with(p)),
                    "{method} {path}: operation id `{id}` uses a Batch 8 prefix"
                );
                continue;
            }
            ops += 1;
            assert!(!id.is_empty(), "{method} {path} has no operationId");
            let ok = RULES.iter().any(|(root, tag, prefixes)| {
                path.starts_with(root)
                    && tags.contains(tag)
                    && prefixes.iter().any(|p| id.starts_with(p))
            });
            assert!(
                ok,
                "{method} {path}: operationId `{id}` does not match the prefixes of its tag {tags:?}"
            );
            if let Some(first) = path.split('/').find(|seg| seg.starts_with('{')) {
                assert_eq!(
                    first, "{id}",
                    "{method} {path}: first path parameter must be {{id}}"
                );
            }
        }
    }
    assert!(ops >= 4, "the Batch 8 foundation endpoints are mounted");
}
