//! Pure grading arithmetic (contract doc §3.5): paper percentage from internal + external marks,
//! grade band lookup, SGPA and CGPA. `rust_decimal` end to end; every computed value is rounded
//! **once** to two places, half away from zero ([`round2`]). No I/O — 6B uses it for the
//! gradebook preview, 6C's computation engine for the official result, so both agree.
//!
//! Rules (ten-point scale, the default until 6A's examination policy makes them configurable):
//!
//! * paper % = internal share + external share, where internal share =
//!   `internal / internal_max × internal_weight` (missing internal counts as 0 when the weight is
//!   positive) and external share = `min(external + grace, external_max) / external_max ×
//!   (100 − internal_weight)`; capped at 100. Absent or no external marks → not gradable (`None`).
//! * grade = the band with the highest `min_percent ≤ paper %`; the paper is **passed** when that
//!   band `is_pass` **and** `paper % ≥ pass_percent`; a paper below `pass_percent` takes the
//!   highest failing band instead. Both comparisons use the **exact** percentage
//!   ([`paper_percent_exact`]); the two-place value is for display / storage only (DEF-I-008).
//! * SGPA = Σ(credits × grade point) / Σ credits over **every** registered line of the term
//!   (ungraded lines count 0 grade points); credits earned = Σ credits of passed lines.
//! * CGPA = Σ grade points / Σ credits attempted over the *effective attempt* of every course
//!   across all published results ([`cumulative`]: a supplementary / improvement attempt replaces
//!   only the course it re-examined, never double-counting credits — DEF-I-005).

use rust_decimal::{Decimal, RoundingStrategy};
use sutra_contracts::exams::ports::GradeBand;
use sutra_kernel::error::{FieldViolation, KernelError};

/// Rounds a computed value to two places, half away from zero.
pub fn round2(v: Decimal) -> Decimal {
    let mut r = v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    r.rescale(2);
    r
}

/// Two-decimal wire string (`"8.45"`).
pub fn fmt2(v: Decimal) -> String {
    round2(v).to_string()
}

fn hundred() -> Decimal {
    Decimal::new(100, 0)
}

/// Inputs of one paper line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineInput {
    pub credits: Decimal,
    pub internal_marks: Option<Decimal>,
    pub internal_max: Option<Decimal>,
    pub external_marks: Option<Decimal>,
    pub external_max: Decimal,
    pub grace_marks: Decimal,
    /// Share of the final percentage taken from internal assessment (0 … 100).
    pub internal_weight: Decimal,
    pub pass_percent: Decimal,
    pub absent: bool,
}

/// A graded line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradedLine {
    /// `None` = not gradable (absent / no external marks).
    pub percent: Option<Decimal>,
    pub grade: Option<String>,
    pub grade_point: Option<Decimal>,
    pub passed: bool,
}

/// Paper percentage rounded for display / storage (see module docs); `None` when not gradable.
/// Pass / band decisions never use this value — they compare [`paper_percent_exact`].
pub fn paper_percent(l: &LineInput) -> Option<Decimal> {
    paper_percent_exact(l).map(round2)
}

/// Exact (unrounded) paper percentage; `None` when not gradable. Pass / fail and the grade band
/// are decided on this value so that e.g. 39.995 % never passes a 40 % paper (DEF-I-008).
pub fn paper_percent_exact(l: &LineInput) -> Option<Decimal> {
    if l.absent || l.external_max <= Decimal::ZERO {
        return None;
    }
    let external = l.external_marks?;
    let weight = l.internal_weight.clamp(Decimal::ZERO, hundred());
    let internal_share = match (l.internal_marks, l.internal_max) {
        (Some(m), Some(max)) if weight > Decimal::ZERO && max > Decimal::ZERO => {
            m.min(max) / max * weight
        }
        _ => Decimal::ZERO,
    };
    let ext = (external + l.grace_marks).min(l.external_max);
    let external_share = ext / l.external_max * (hundred() - weight);
    Some((internal_share + external_share).min(hundred()))
}

/// The band for `percent` (highest `min_percent ≤ percent`).
pub fn band_for(percent: Decimal, bands: &[GradeBand]) -> Option<&GradeBand> {
    bands
        .iter()
        .filter(|b| b.min_percent <= percent)
        .max_by_key(|b| b.min_percent)
}

/// Grades one line against a scheme's bands.
pub fn grade_line(l: &LineInput, bands: &[GradeBand]) -> GradedLine {
    let Some(percent) = paper_percent_exact(l) else {
        return GradedLine {
            percent: None,
            grade: None,
            grade_point: None,
            passed: false,
        };
    };
    let band = band_for(percent, bands);
    let passed = band.is_some_and(|b| b.is_pass) && percent >= l.pass_percent;
    let band = if passed {
        band
    } else {
        // Below the paper's pass mark: the highest failing band applies.
        bands
            .iter()
            .filter(|b| !b.is_pass && b.min_percent <= percent)
            .max_by_key(|b| b.min_percent)
            .or_else(|| {
                bands
                    .iter()
                    .filter(|b| !b.is_pass)
                    .min_by_key(|b| b.min_percent)
            })
    };
    GradedLine {
        // Rounded for display only; the decision above used the exact value.
        percent: Some(round2(percent)),
        grade: band.map(|b| b.grade.clone()),
        grade_point: Some(band.map_or(Decimal::ZERO, |b| b.grade_point)),
        passed,
    }
}

/// Term aggregates of a set of graded lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermAggregate {
    pub credits_attempted: Decimal,
    pub credits_earned: Decimal,
    /// Σ credits × grade point.
    pub grade_points: Decimal,
    /// `None` when no credits were attempted.
    pub sgpa: Option<Decimal>,
}

/// SGPA over `(credits, graded line)` pairs (see module docs).
pub fn aggregate(lines: &[(Decimal, GradedLine)]) -> TermAggregate {
    let mut attempted = Decimal::ZERO;
    let mut earned = Decimal::ZERO;
    let mut points = Decimal::ZERO;
    for (credits, g) in lines {
        attempted += *credits;
        if g.passed {
            earned += *credits;
        }
        points += *credits * g.grade_point.unwrap_or(Decimal::ZERO);
    }
    let sgpa = (attempted > Decimal::ZERO).then(|| round2(points / attempted));
    TermAggregate {
        credits_attempted: round2(attempted),
        credits_earned: round2(earned),
        grade_points: round2(points),
        sgpa,
    }
}

/// CGPA over `(credits_attempted, grade_points)` of every term's current result.
pub fn cgpa(terms: &[(Decimal, Decimal)]) -> Option<Decimal> {
    let credits: Decimal = terms.iter().map(|t| t.0).sum();
    let points: Decimal = terms.iter().map(|t| t.1).sum();
    (credits > Decimal::ZERO).then(|| round2(points / credits))
}

/// One graded line of a published attempt, as input to the cumulative (CGPA / completion)
/// calculation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptLine {
    /// The curriculum component (course) the paper examines.
    pub component: uuid::Uuid,
    pub credits: Decimal,
    /// `None` = not graded (absent): counts 0 grade points.
    pub grade_point: Option<Decimal>,
    pub passed: bool,
}

/// Cumulative standing over every attempt of every course.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cumulative {
    pub credits_attempted: Decimal,
    pub credits_earned: Decimal,
    pub grade_points: Decimal,
    pub cgpa: Option<Decimal>,
    /// Courses with an effective passing attempt.
    pub passed_components: std::collections::HashSet<uuid::Uuid>,
}

/// Cumulative CGPA / credits under the grade-replacement policy (DEF-I-005).
///
/// `attempts` are the lines of every published result of the student, **oldest attempt
/// first** (regular session, then supplementary / improvement sessions in the order they were
/// first published). Each course (curriculum component) counts **once**, with its *effective*
/// attempt:
///
/// * if any attempt passed, the passing attempt with the highest grade point (ties → the later
///   one) — a supplementary pass replaces the failed regular attempt, an improvement attempt
///   only ever improves the grade;
/// * otherwise the latest attempt (a failed re-attempt keeps the course at 0 points, counted
///   once).
///
/// A supplementary result therefore replaces only the course(s) it re-examined; the other
/// courses of the term keep their regular grades, and no credit is counted twice.
/// CGPA = Σ(credits × grade point) / Σ credits over the effective attempts, rounded once.
pub fn cumulative(attempts: &[AttemptLine]) -> Cumulative {
    let mut order: Vec<uuid::Uuid> = Vec::new();
    let mut effective: std::collections::HashMap<uuid::Uuid, &AttemptLine> =
        std::collections::HashMap::new();
    for a in attempts {
        match effective.get(&a.component) {
            None => {
                order.push(a.component);
                effective.insert(a.component, a);
            }
            Some(cur) => {
                let gp = |l: &AttemptLine| l.grade_point.unwrap_or(Decimal::ZERO);
                let replace = match (cur.passed, a.passed) {
                    (false, _) => true,
                    (true, false) => false,
                    (true, true) => gp(a) >= gp(cur),
                };
                if replace {
                    effective.insert(a.component, a);
                }
            }
        }
    }
    let mut attempted = Decimal::ZERO;
    let mut earned = Decimal::ZERO;
    let mut points = Decimal::ZERO;
    let mut passed = std::collections::HashSet::new();
    for c in &order {
        let l = effective[c];
        attempted += l.credits;
        points += l.credits * l.grade_point.unwrap_or(Decimal::ZERO);
        if l.passed {
            earned += l.credits;
            passed.insert(*c);
        }
    }
    Cumulative {
        credits_attempted: round2(attempted),
        credits_earned: round2(earned),
        grade_points: round2(points),
        cgpa: (attempted > Decimal::ZERO).then(|| round2(points / attempted)),
        passed_components: passed,
    }
}

/// Validates a grading scheme's bands → 422 on `bands` otherwise: at least one pass and one
/// failing band, unique grades and minimums, a band starting at 0 %, grade points 0 … 10, and
/// grade points never higher for a lower band.
pub fn validate_bands(bands: &[GradeBand]) -> Result<(), KernelError> {
    let fail = |code: &str, msg: &str| {
        Err(KernelError::Validation(vec![FieldViolation::new(
            "bands", code, msg,
        )]))
    };
    if bands.len() < 2 || bands.len() > 20 {
        return fail("count", "a scheme needs between 2 and 20 bands");
    }
    let mut sorted: Vec<&GradeBand> = bands.iter().collect();
    sorted.sort_by_key(|b| b.min_percent);
    if sorted
        .first()
        .is_none_or(|b| b.min_percent != Decimal::ZERO)
    {
        return fail("no_floor", "one band must start at 0 %");
    }
    for w in sorted.windows(2) {
        if w[0].min_percent == w[1].min_percent {
            return fail("duplicate_min", "band minimums must be unique");
        }
        if w[0].grade_point > w[1].grade_point {
            return fail(
                "not_monotonic",
                "a higher band cannot carry fewer grade points",
            );
        }
    }
    let mut grades: Vec<&str> = bands.iter().map(|b| b.grade.as_str()).collect();
    grades.sort_unstable();
    grades.dedup();
    if grades.len() != bands.len() {
        return fail("duplicate_grade", "grades must be unique");
    }
    if !bands.iter().any(|b| b.is_pass) || !bands.iter().any(|b| !b.is_pass) {
        return fail(
            "pass_fail",
            "a scheme needs at least one pass and one failing band",
        );
    }
    let ten = Decimal::new(10, 0);
    if bands.iter().any(|b| {
        b.min_percent < Decimal::ZERO
            || b.min_percent > hundred()
            || b.grade_point < Decimal::ZERO
            || b.grade_point > ten
    }) {
        return fail("range", "minimums are 0 … 100 % and grade points 0 … 10");
    }
    Ok(())
}

/// The default UGC-style ten-point bands (O, A+, A, B+, B, C, P, F) — used by tests and as the
/// web form's starting point.
pub fn ten_point_default() -> Vec<GradeBand> {
    [
        ("O", 90, 10, true),
        ("A+", 80, 9, true),
        ("A", 70, 8, true),
        ("B+", 60, 7, true),
        ("B", 55, 6, true),
        ("C", 50, 5, true),
        ("P", 40, 4, true),
        ("F", 0, 0, false),
    ]
    .into_iter()
    .map(|(g, min, gp, pass)| GradeBand {
        grade: g.to_owned(),
        min_percent: Decimal::new(min, 0),
        grade_point: Decimal::new(gp, 0),
        is_pass: pass,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn line(internal: Option<&str>, external: Option<&str>, weight: &str) -> LineInput {
        LineInput {
            credits: d("4"),
            internal_marks: internal.map(d),
            internal_max: Some(d("40")),
            external_marks: external.map(d),
            external_max: d("100"),
            grace_marks: Decimal::ZERO,
            internal_weight: d(weight),
            pass_percent: d("40"),
            absent: false,
        }
    }

    #[test]
    fn percent_combines_internal_and_external_shares() {
        // 30/40 × 40 + 60/100 × 60 = 30 + 36 = 66
        assert_eq!(
            paper_percent(&line(Some("30"), Some("60"), "40")),
            Some(d("66.00"))
        );
        // no internal weight: external only
        assert_eq!(
            paper_percent(&line(Some("30"), Some("72.5"), "0")),
            Some(d("72.50"))
        );
        // missing internal counts 0
        assert_eq!(
            paper_percent(&line(None, Some("50"), "40")),
            Some(d("30.00"))
        );
        // absent / no external: not gradable
        let mut a = line(Some("30"), Some("60"), "40");
        a.absent = true;
        assert_eq!(paper_percent(&a), None);
        assert_eq!(paper_percent(&line(Some("30"), None, "40")), None);
        // grace is capped at the external maximum
        let mut g = line(None, Some("98"), "0");
        g.grace_marks = d("5");
        assert_eq!(paper_percent(&g), Some(d("100.00")));
        // rounding half away from zero: 1/3 × 100
        let mut t = line(None, Some("1"), "0");
        t.external_max = d("3");
        assert_eq!(paper_percent(&t), Some(d("33.33")));
    }

    #[test]
    fn grades_follow_bands_and_the_pass_mark() {
        let bands = ten_point_default();
        let g = grade_line(&line(Some("30"), Some("60"), "40"), &bands);
        assert_eq!(
            (g.grade.as_deref(), g.grade_point, g.passed),
            (Some("B+"), Some(d("7")), true)
        );
        let f = grade_line(&line(None, Some("35"), "0"), &bands);
        assert_eq!((f.grade.as_deref(), f.passed), (Some("F"), false));
        // band passes at 40 % but the paper's own pass mark is 50 %
        let mut strict = line(None, Some("45"), "0");
        strict.pass_percent = d("50");
        let s = grade_line(&strict, &bands);
        assert_eq!(
            (s.grade.as_deref(), s.grade_point, s.passed),
            (Some("F"), Some(d("0")), false)
        );
        let absent = grade_line(&line(None, None, "0"), &bands);
        assert_eq!(
            (absent.grade, absent.grade_point, absent.passed),
            (None, None, false)
        );
    }

    #[test]
    fn sgpa_and_cgpa_are_credit_weighted() {
        let bands = ten_point_default();
        let lines = vec![
            (d("4"), grade_line(&line(None, Some("92"), "0"), &bands)), // O 10
            (d("3"), grade_line(&line(None, Some("75"), "0"), &bands)), // A 8
            (d("3"), grade_line(&line(None, Some("20"), "0"), &bands)), // F 0
        ];
        let t = aggregate(&lines);
        assert_eq!(t.credits_attempted, d("10.00"));
        assert_eq!(t.credits_earned, d("7.00"));
        assert_eq!(t.grade_points, d("64.00"));
        assert_eq!(t.sgpa, Some(d("6.40")));
        assert_eq!(aggregate(&[]).sgpa, None);
        assert_eq!(
            cgpa(&[(d("10"), d("64")), (d("20"), d("170"))]),
            Some(d("7.80"))
        );
        assert_eq!(cgpa(&[]), None);
        assert_eq!(fmt2(d("8")), "8.00");
    }

    #[test]
    fn pass_and_band_are_decided_on_the_exact_percentage() {
        let bands = ten_point_default();
        // 39.995 % (external 79.99 / 200): displayed as 40.00 but below the 40 % pass mark.
        let mut l = line(None, Some("79.99"), "0");
        l.external_max = d("200");
        assert_eq!(paper_percent_exact(&l), Some(d("39.995")));
        assert_eq!(paper_percent(&l), Some(d("40.00")));
        let g = grade_line(&l, &bands);
        assert_eq!(
            (g.percent, g.grade.as_deref(), g.passed),
            (Some(d("40.00")), Some("F"), false)
        );
        // exactly 40 % passes.
        let mut p = line(None, Some("80"), "0");
        p.external_max = d("200");
        let g = grade_line(&p, &bands);
        assert_eq!((g.grade.as_deref(), g.passed), (Some("P"), true));
        // 89.995 % displays 90.00 but stays in the A+ band, not O.
        let mut b = line(None, Some("179.99"), "0");
        b.external_max = d("200");
        let g = grade_line(&b, &bands);
        assert_eq!(
            (g.percent, g.grade.as_deref()),
            (Some(d("90.00")), Some("A+"))
        );
    }

    #[test]
    fn cumulative_replaces_only_the_reattempted_course() {
        let (a, b, c, e) = (
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        );
        let l = |comp, cr: &str, gp: &str, passed| AttemptLine {
            component: comp,
            credits: d(cr),
            grade_point: Some(d(gp)),
            passed,
        };
        // Term 1: A 4cr O(10), B 3cr F(0); term 2: C 4cr A(8);
        // supplementary for B: 3cr B+(7); a failed then passed course E (2cr F → P 4).
        let attempts = vec![
            l(a, "4", "10", true),
            l(b, "3", "0", false),
            l(c, "4", "8", true),
            l(e, "2", "0", false),
            l(b, "3", "7", true),
            l(e, "2", "4", true),
        ];
        let cu = cumulative(&attempts);
        // effective: A 40, B 21, C 32, E 8 → 101 / 13 = 7.769… → 7.77
        assert_eq!(cu.credits_attempted, d("13.00"));
        assert_eq!(cu.credits_earned, d("13.00"));
        assert_eq!(cu.grade_points, d("101.00"));
        assert_eq!(cu.cgpa, Some(d("7.77")));
        assert_eq!(cu.passed_components.len(), 4);
        // A failed supplementary keeps the course once, at 0 points.
        let failed_again = vec![
            l(a, "4", "10", true),
            l(b, "3", "0", false),
            l(b, "3", "0", false),
        ];
        let cu = cumulative(&failed_again);
        assert_eq!(cu.credits_attempted, d("7.00"));
        assert_eq!(cu.credits_earned, d("4.00"));
        assert_eq!(cu.cgpa, Some(d("5.71")));
        // An improvement attempt that scores lower never lowers the grade.
        let worse = vec![l(a, "4", "9", true), l(a, "4", "6", true)];
        assert_eq!(cumulative(&worse).cgpa, Some(d("9.00")));
        // A later failed attempt never replaces an earlier pass.
        let regress = vec![l(a, "4", "7", true), l(a, "4", "0", false)];
        let cu = cumulative(&regress);
        assert_eq!((cu.cgpa, cu.credits_earned), (Some(d("7.00")), d("4.00")));
        assert_eq!(cumulative(&[]).cgpa, None);
    }

    #[test]
    fn band_validation_rejects_broken_schemes() {
        assert!(validate_bands(&ten_point_default()).is_ok());
        let mut no_floor = ten_point_default();
        no_floor.pop();
        assert!(validate_bands(&no_floor).is_err());
        let mut dup = ten_point_default();
        dup[1].grade = "O".into();
        assert!(validate_bands(&dup).is_err());
        let mut inverted = ten_point_default();
        inverted[0].grade_point = d("3");
        assert!(validate_bands(&inverted).is_err());
        let all_pass: Vec<_> = ten_point_default()
            .into_iter()
            .map(|mut b| {
                b.is_pass = true;
                b
            })
            .collect();
        assert!(validate_bands(&all_pass).is_err());
    }
}
