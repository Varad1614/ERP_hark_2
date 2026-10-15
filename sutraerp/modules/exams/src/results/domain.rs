//! Pure business rules of slice 6C results: the result-batch and publication machines, the
//! malpractice consequence on the computation input (L3-I-095/112) and the per-candidate
//! computation (final marks → graded lines → SGPA / outcome, withheld conditions — L3-I-083/085)
//! on top of the foundation grading engine (`shared::grading`, contract §3.5).

use crate::shared::grading::{aggregate, grade_line, GradedLine, LineInput, TermAggregate};
use crate::shared::results::{NewResultLine, ResultRow};
use rust_decimal::Decimal;
use std::collections::HashMap;
use sutra_contracts::exams::ids::ExamPaperId;
use sutra_contracts::exams::ports::{
    CandidateSummary, ExamPaperSummary, GradeBand, PaperMarkSummary,
};
use sutra_contracts::exams::status::{
    MalpracticePenalty, PublicationStatus, RegistrationPaperStatus, ResultBatchStatus,
    ResultOutcome,
};

/// Legal moves of a result batch (L2-I09 §20). `pending → computed` and `computed → computed`
/// stand for one computation run (aggregation and rule processing happen inside it);
/// `validated / approved / finalised → computed` is a governed reopening before publication
/// (L3-I-087/099).
pub fn batch_can_transition(from: ResultBatchStatus, to: ResultBatchStatus) -> bool {
    use ResultBatchStatus::*;
    matches!(
        (from, to),
        (Pending, Aggregated)
            | (Aggregated, RuleProcessing)
            | (RuleProcessing, Computed)
            | (Pending, Computed)
            | (Computed, Computed)
            | (Computed, Validated)
            | (Validated, Computed)
            | (Validated, Approved)
            | (Approved, Computed)
            | (Approved, Finalised)
            | (Finalised, Computed)
            | (Finalised, ReadyForPublication)
            | (ReadyForPublication, Computed)
            | (ReadyForPublication, Closed)
            | (Closed, Archived)
    )
}

/// Batch states whose results may still be recomputed in place.
pub fn batch_recomputable(s: ResultBatchStatus) -> bool {
    matches!(
        s,
        ResultBatchStatus::Pending | ResultBatchStatus::Computed | ResultBatchStatus::Validated
    )
}

/// Batch states after institutional approval (a released result needs a release approval
/// unless it returns unchanged to the approval it already had — DEF-I-003).
pub fn batch_approved(s: ResultBatchStatus) -> bool {
    matches!(
        s,
        ResultBatchStatus::Approved
            | ResultBatchStatus::Finalised
            | ResultBatchStatus::ReadyForPublication
            | ResultBatchStatus::Closed
    )
}

/// Legal moves of a publication (L2-I10 §20 + delay / cancellation / failure, L3-I-104/105/116).
pub fn publication_can_transition(from: PublicationStatus, to: PublicationStatus) -> bool {
    use PublicationStatus::*;
    matches!(
        (from, to),
        (Planned, Scheduled)
            | (Planned, Authorised)
            | (Scheduled, Authorised)
            | (Authorised, Ready)
            | (Authorised, Published)
            | (Ready, Published)
            | (Published, Accessible)
            | (Planned | Scheduled | Authorised | Ready, Delayed)
            | (Delayed, Planned)
            | (Delayed, Authorised)
            | (
                Planned | Scheduled | Authorised | Ready | Delayed | Failed,
                Cancelled
            )
            | (Authorised | Ready, Failed)
            | (Failed, Published)
            | (Published | Accessible, Withdrawn)
            | (Published | Accessible | Cancelled | Withdrawn, Closed)
            | (Closed, Archived)
    )
}

/// A decided malpractice penalty against a candidate (6B `malpractice_case` rows in status
/// `decided | upheld | modified`). `paper_id = None` applies to every paper of the registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecidedPenalty {
    pub paper_id: Option<ExamPaperId>,
    pub penalty: MalpracticePenalty,
    pub marks_deduction: Option<Decimal>,
}

/// Effect of malpractice penalties on one paper's computation input (contract §7): marks
/// deductions reduce the external marks (computation input only, never `paper_mark`); a
/// cancelled paper / session or a debarment makes the paper not passed; any penalty above a
/// warning makes the outcome `malpractice`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MalpracticeEffect {
    pub deduction: Decimal,
    pub cancelled: bool,
    pub malpractice: bool,
}

/// Pure malpractice consequence for `paper` (L3-I-095 / L3-I-112).
pub fn malpractice_consequence(
    paper: ExamPaperId,
    penalties: &[DecidedPenalty],
) -> MalpracticeEffect {
    let mut e = MalpracticeEffect::default();
    for p in penalties
        .iter()
        .filter(|p| p.paper_id.is_none_or(|id| id == paper))
    {
        match p.penalty {
            MalpracticePenalty::None | MalpracticePenalty::Warning => {}
            MalpracticePenalty::MarksDeduction => {
                e.deduction += p.marks_deduction.unwrap_or(Decimal::ZERO);
                e.malpractice = true;
            }
            MalpracticePenalty::SessionCancelled
            | MalpracticePenalty::PaperCancelled
            | MalpracticePenalty::Debarred => {
                e.cancelled = true;
                e.malpractice = true;
            }
        }
    }
    e
}

/// The computed result of one candidate (before CGPA, which needs the other terms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateComputation {
    pub lines: Vec<NewResultLine>,
    pub aggregate: TermAggregate,
    pub outcome: ResultOutcome,
    /// Non-empty → the result is `withheld` with these reasons (L3-I-085).
    pub withheld: Vec<String>,
}

impl CandidateComputation {
    pub fn withheld_reason(&self) -> Option<String> {
        (!self.withheld.is_empty()).then(|| self.withheld.join("; "))
    }
}

/// Computes one candidate from final marks (L3-I-083/085/095/114). Only `finalised` marks are
/// inputs; any other mark row of a registered paper blocks the result (withheld). An open
/// malpractice case (`open_cases`: case numbers still under investigation / hearing / appeal)
/// withholds the result too (contract §1 6C "pending malpractice", DEF-I-004).
pub fn compute_candidate(
    reg: &CandidateSummary,
    papers: &HashMap<ExamPaperId, ExamPaperSummary>,
    marks: &[PaperMarkSummary],
    bands: &[GradeBand],
    penalties: &[DecidedPenalty],
    open_cases: &[String],
) -> CandidateComputation {
    let mut lines = Vec::new();
    let mut graded: Vec<(Decimal, GradedLine)> = Vec::new();
    let mut withheld = Vec::new();
    if !open_cases.is_empty() {
        withheld.push(format!(
            "malpractice case {} pending",
            open_cases.join(", ")
        ));
    }
    let mut malpractice = false;
    let mut any_absent = false;
    for rp in reg
        .papers
        .iter()
        .filter(|p| p.status != RegistrationPaperStatus::Withdrawn)
    {
        let Some(paper) = papers.get(&rp.paper_id) else {
            withheld.push("a registered paper is no longer part of the session".to_owned());
            continue;
        };
        let Some(mark) = marks
            .iter()
            .find(|m| m.registration_paper_id == rp.registration_paper_id)
        else {
            withheld.push(format!("no marks recorded for {}", paper.paper_code));
            continue;
        };
        let debarred = rp.status == RegistrationPaperStatus::Debarred;
        if !mark.status.is_final() && !debarred {
            withheld.push(format!(
                "marks for {} are {} (not final)",
                paper.paper_code, mark.status
            ));
            continue;
        }
        let mut effect = malpractice_consequence(paper.id, penalties);
        if debarred {
            effect.cancelled = true;
            effect.malpractice = true;
        }
        malpractice |= effect.malpractice;
        let absent = mark.absent || rp.status == RegistrationPaperStatus::Absent;
        any_absent |= absent;
        let external = mark
            .external_marks
            .map(|e| (e - effect.deduction).max(Decimal::ZERO));
        let input = LineInput {
            credits: paper.credits,
            internal_marks: mark.internal_marks,
            internal_max: mark.internal_max,
            external_marks: external,
            external_max: mark.external_max,
            grace_marks: mark.grace_marks,
            internal_weight: paper.internal_weight,
            pass_percent: paper.pass_percent,
            absent,
        };
        let mut g = grade_line(&input, bands);
        let remark = if effect.cancelled {
            g = GradedLine {
                percent: None,
                grade: None,
                grade_point: Some(Decimal::ZERO),
                passed: false,
            };
            Some(if debarred {
                "debarred (malpractice)".to_owned()
            } else {
                "cancelled (malpractice)".to_owned()
            })
        } else if absent {
            Some("absent".to_owned())
        } else if effect.deduction > Decimal::ZERO {
            Some(format!("malpractice deduction {}", effect.deduction))
        } else {
            None
        };
        let total = g.percent.map(|_| {
            mark.internal_marks.unwrap_or(Decimal::ZERO)
                + (external.unwrap_or(Decimal::ZERO) + mark.grace_marks).min(mark.external_max)
        });
        lines.push(NewResultLine {
            paper_id: paper.id,
            curriculum_component_id: paper.curriculum_component_id,
            paper_code: paper.paper_code.clone(),
            title: paper.title.clone(),
            credits: paper.credits,
            attempt_kind: rp.attempt_kind,
            internal_marks: mark.internal_marks,
            external_marks: external,
            grace_marks: mark.grace_marks,
            total_marks: total,
            max_marks: mark.external_max + mark.internal_max.unwrap_or(Decimal::ZERO),
            percent: g.percent,
            grade: g.grade.clone(),
            grade_point: g.grade_point,
            passed: g.passed,
            remark,
        });
        graded.push((paper.credits, g));
    }
    let agg = aggregate(&graded);
    let outcome = if !withheld.is_empty() {
        ResultOutcome::Withheld
    } else if malpractice {
        ResultOutcome::Malpractice
    } else if lines.is_empty() {
        ResultOutcome::Incomplete
    } else if lines.iter().all(|l| l.passed) {
        ResultOutcome::Pass
    } else if any_absent
        && lines
            .iter()
            .all(|l| l.passed || l.remark.as_deref() == Some("absent"))
    {
        // Absence consequence (L3-I-114): the paper is not passed; the term is incomplete.
        ResultOutcome::Incomplete
    } else {
        ResultOutcome::Fail
    };
    CandidateComputation {
        lines: if withheld.is_empty() {
            lines
        } else {
            Vec::new()
        },
        aggregate: if withheld.is_empty() {
            agg
        } else {
            aggregate(&[])
        },
        outcome,
        withheld,
    }
}

/// `true` when a stored result already carries exactly this computation (idempotent re-runs,
/// L3-I-093: no new revision for an unchanged outcome).
pub fn same_content(r: &ResultRow, c: &CandidateComputation, cgpa: Option<Decimal>) -> bool {
    r.outcome == c.outcome
        && r.credits_attempted == c.aggregate.credits_attempted
        && r.credits_earned == c.aggregate.credits_earned
        && r.grade_points == c.aggregate.grade_points
        && r.sgpa == c.aggregate.sgpa
        && r.cgpa == cgpa
        && r.lines.len() == c.lines.len()
        && r.lines.iter().zip(&c.lines).all(|(a, b)| {
            a.paper_id == b.paper_id
                && a.internal_marks == b.internal_marks
                && a.external_marks == b.external_marks
                && a.grace_marks == b.grace_marks
                && a.percent == b.percent
                && a.grade == b.grade
                && a.grade_point == b.grade_point
                && a.passed == b.passed
                && a.remark == b.remark
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::grading::ten_point_default;
    use std::str::FromStr;
    use sutra_contracts::exams::ids::{
        ExamRegistrationId, ExamRegistrationPaperId, ExamSessionId, PaperMarkId,
    };
    use sutra_contracts::exams::ports::CandidatePaper;
    use sutra_contracts::exams::status::{
        AttemptKind, ExamMode, ExamPaperStatus, ExamRegistrationStatus, MarkStatus,
        RegistrationChannel,
    };
    use sutra_contracts::student::ids::{StudentContextId, StudentId};
    use sutra_kernel::scope::InstitutionId;
    use uuid::Uuid;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    struct Fixture {
        reg: CandidateSummary,
        papers: HashMap<ExamPaperId, ExamPaperSummary>,
        marks: Vec<PaperMarkSummary>,
    }

    fn fixture(specs: &[(&str, Option<&str>, MarkStatus)]) -> Fixture {
        let session = ExamSessionId(Uuid::nil());
        let mut papers = HashMap::new();
        let mut cps = Vec::new();
        let mut marks = Vec::new();
        for (credits, external, status) in specs {
            let pid = ExamPaperId(Uuid::now_v7());
            let rpid = ExamRegistrationPaperId(Uuid::now_v7());
            papers.insert(
                pid,
                ExamPaperSummary {
                    id: pid,
                    session_id: session,
                    institution_id: InstitutionId(Uuid::nil()),
                    offering_id: None,
                    curriculum_component_id: Uuid::now_v7(),
                    academic_term_id: Uuid::nil(),
                    paper_code: format!("P{}", papers.len() + 1),
                    title: "Paper".into(),
                    credits: d(credits),
                    mode: ExamMode::Theory,
                    max_marks: d("100"),
                    pass_percent: d("40"),
                    internal_weight: d("0"),
                    duration_minutes: None,
                    status: ExamPaperStatus::MarksFinalised,
                    version: 1,
                },
            );
            cps.push(CandidatePaper {
                registration_paper_id: rpid,
                paper_id: pid,
                attempt_kind: AttemptKind::Regular,
                attempt_no: 1,
                status: RegistrationPaperStatus::Appeared,
            });
            marks.push(PaperMarkSummary {
                id: PaperMarkId(Uuid::now_v7()),
                session_id: session,
                paper_id: pid,
                registration_id: ExamRegistrationId(Uuid::nil()),
                registration_paper_id: rpid,
                student_id: StudentId(Uuid::nil()),
                internal_marks: None,
                internal_max: None,
                external_marks: external.map(d),
                external_max: d("100"),
                grace_marks: Decimal::ZERO,
                absent: external.is_none(),
                status: *status,
                internal_source_id: None,
                version: 1,
            });
        }
        Fixture {
            reg: CandidateSummary {
                registration_id: ExamRegistrationId(Uuid::nil()),
                session_id: session,
                institution_id: InstitutionId(Uuid::nil()),
                student_id: StudentId(Uuid::nil()),
                student_context_id: StudentContextId(Uuid::nil()),
                registration_number: "EXR-000001".into(),
                student_number: "S1".into(),
                display_name: "S".into(),
                subject_ref: None,
                channel: RegistrationChannel::Administrative,
                status: ExamRegistrationStatus::Confirmed,
                papers: cps,
                version: 1,
            },
            papers,
            marks,
        }
    }

    #[test]
    fn final_marks_produce_sgpa_and_outcome() {
        let f = fixture(&[
            ("4", Some("92"), MarkStatus::Finalised),
            ("3", Some("75"), MarkStatus::Finalised),
            ("3", Some("20"), MarkStatus::Finalised),
        ]);
        let c = compute_candidate(&f.reg, &f.papers, &f.marks, &ten_point_default(), &[], &[]);
        assert!(c.withheld.is_empty());
        assert_eq!(c.aggregate.sgpa, Some(d("6.40")));
        assert_eq!(c.aggregate.credits_earned, d("7.00"));
        assert_eq!(c.outcome, ResultOutcome::Fail);
        assert_eq!(c.lines.len(), 3);
        assert_eq!(c.lines[0].grade.as_deref(), Some("O"));
    }

    #[test]
    fn non_final_marks_withhold_the_result() {
        let f = fixture(&[
            ("4", Some("92"), MarkStatus::Finalised),
            ("3", Some("75"), MarkStatus::Verified),
        ]);
        let c = compute_candidate(&f.reg, &f.papers, &f.marks, &ten_point_default(), &[], &[]);
        assert_eq!(c.outcome, ResultOutcome::Withheld);
        assert!(c.lines.is_empty());
        assert!(c.withheld_reason().is_some_and(|r| r.contains("verified")));
    }

    #[test]
    fn an_open_malpractice_case_withholds_the_result() {
        let f = fixture(&[("4", Some("92"), MarkStatus::Finalised)]);
        let c = compute_candidate(
            &f.reg,
            &f.papers,
            &f.marks,
            &ten_point_default(),
            &[],
            &["MC-000001".to_owned()],
        );
        assert_eq!(c.outcome, ResultOutcome::Withheld);
        assert!(c.lines.is_empty());
        assert_eq!(
            c.withheld_reason().as_deref(),
            Some("malpractice case MC-000001 pending")
        );
    }

    #[test]
    fn absence_makes_the_term_incomplete() {
        let f = fixture(&[
            ("4", Some("92"), MarkStatus::Finalised),
            ("3", None, MarkStatus::Finalised),
        ]);
        let c = compute_candidate(&f.reg, &f.papers, &f.marks, &ten_point_default(), &[], &[]);
        assert_eq!(c.outcome, ResultOutcome::Incomplete);
        assert_eq!(c.lines[1].remark.as_deref(), Some("absent"));
        assert_eq!(c.aggregate.credits_attempted, d("7.00"));
    }

    #[test]
    fn malpractice_penalties_change_the_computation_input_only() {
        let f = fixture(&[
            ("4", Some("62"), MarkStatus::Finalised),
            ("3", Some("75"), MarkStatus::Finalised),
        ]);
        let first = f.reg.papers[0].paper_id;
        let second = f.reg.papers[1].paper_id;
        let deduction = [DecidedPenalty {
            paper_id: Some(first),
            penalty: MalpracticePenalty::MarksDeduction,
            marks_deduction: Some(d("25")),
        }];
        let c = compute_candidate(
            &f.reg,
            &f.papers,
            &f.marks,
            &ten_point_default(),
            &deduction,
            &[],
        );
        assert_eq!(c.outcome, ResultOutcome::Malpractice);
        assert_eq!(c.lines[0].external_marks, Some(d("37")));
        assert!(!c.lines[0].passed);
        assert!(c.lines[1].passed);
        let cancelled = [DecidedPenalty {
            paper_id: Some(second),
            penalty: MalpracticePenalty::PaperCancelled,
            marks_deduction: None,
        }];
        let e = malpractice_consequence(second, &cancelled);
        assert!(e.cancelled && e.malpractice);
        assert_eq!(
            malpractice_consequence(first, &cancelled),
            MalpracticeEffect::default()
        );
        let session = [DecidedPenalty {
            paper_id: None,
            penalty: MalpracticePenalty::SessionCancelled,
            marks_deduction: None,
        }];
        assert!(malpractice_consequence(first, &session).cancelled);
        let warning = [DecidedPenalty {
            paper_id: None,
            penalty: MalpracticePenalty::Warning,
            marks_deduction: None,
        }];
        let c = compute_candidate(
            &f.reg,
            &f.papers,
            &f.marks,
            &ten_point_default(),
            &warning,
            &[],
        );
        assert_eq!(c.outcome, ResultOutcome::Pass);
        // Debarred registered paper (6B sets it): not passed, outcome malpractice.
        let mut g = fixture(&[("4", Some("80"), MarkStatus::Finalised)]);
        g.reg.papers[0].status = RegistrationPaperStatus::Debarred;
        let c = compute_candidate(&g.reg, &g.papers, &g.marks, &ten_point_default(), &[], &[]);
        assert_eq!(c.outcome, ResultOutcome::Malpractice);
        assert!(!c.lines[0].passed);
    }

    #[test]
    fn machines_allow_only_the_documented_moves() {
        use PublicationStatus as P;
        use ResultBatchStatus as B;
        assert!(batch_can_transition(B::Pending, B::Computed));
        assert!(batch_can_transition(B::Validated, B::Approved));
        assert!(!batch_can_transition(B::Computed, B::Approved));
        assert!(!batch_can_transition(B::Closed, B::Computed));
        assert!(publication_can_transition(P::Authorised, P::Published));
        assert!(publication_can_transition(P::Failed, P::Published));
        assert!(!publication_can_transition(P::Planned, P::Published));
        assert!(!publication_can_transition(P::Cancelled, P::Published));
        assert!(batch_recomputable(B::Validated) && !batch_recomputable(B::Approved));
        assert!(batch_approved(B::ReadyForPublication));
    }
}
