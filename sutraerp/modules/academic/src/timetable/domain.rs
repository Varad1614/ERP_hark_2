//! Pure timetable rules (L2-D03 §12.4, L3-D-020…022): the lifecycle, conflict validation and
//! the greedy draft generator. No I/O; unit-tested here.

use chrono::NaiveTime;
use sutra_contracts::academic::status::TimetableStatus;
use uuid::Uuid;

/// Legal timetable transitions (409 otherwise).
pub fn can_transition(from: TimetableStatus, to: TimetableStatus) -> bool {
    use TimetableStatus::*;
    matches!(
        (from, to),
        (Planning, Draft)
            | (Draft, Validated)
            | (Validated, Draft)
            | (Validated, Approved)
            | (Approved, Published)
            | (Published, Active)
            | (Active, Frozen)
            | (Frozen, Active)
            | (Active, Revised)
            | (Frozen, Revised)
            | (Active, Retired)
            | (Frozen, Retired)
            | (Revised, Archived)
            | (Retired, Archived)
            // a withdrawn revision draft is archived without ever being published
            | (Planning, Archived)
            | (Draft, Archived)
            | (Validated, Archived)
    )
}

/// Slots may change only while the timetable is being planned.
pub fn slots_editable(s: TimetableStatus) -> bool {
    matches!(s, TimetableStatus::Planning | TimetableStatus::Draft)
}

/// One slot as validation sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotFacts {
    pub id: Uuid,
    pub section_id: Uuid,
    pub section_label: String,
    pub day_of_week: i16,
    pub starts_at: NaiveTime,
    pub ends_at: NaiveTime,
    pub room_label: Option<String>,
    /// Login subjects of the faculty assigned to the section.
    pub faculty: Vec<String>,
}

/// A section's scheduling facts (validation + generation).
#[derive(Clone, Debug, PartialEq)]
pub struct SectionFacts {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub label: String,
    /// Offering deliverable or still planned (not cancelled / terminal) and section not ended.
    pub schedulable: bool,
    pub sessions_per_week: Option<i16>,
    pub session_minutes: i16,
    pub preferred_days: Vec<i16>,
    pub room_label: Option<String>,
    pub faculty: Vec<String>,
}

/// One validation finding.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// `section_overlap`, `faculty_overlap`, `room_overlap`, `section_not_schedulable`,
    /// `requirement_shortfall`, `empty_timetable`.
    pub code: String,
    /// `critical` blocks approval; `warning` does not.
    pub severity: String,
    pub message: String,
    pub slot_ids: Vec<Uuid>,
}

fn overlaps(a: &SlotFacts, b: &SlotFacts) -> bool {
    a.day_of_week == b.day_of_week && a.starts_at < b.ends_at && b.starts_at < a.ends_at
}

fn critical(code: &str, message: String, slot_ids: Vec<Uuid>) -> Conflict {
    Conflict {
        code: code.into(),
        severity: "critical".into(),
        message,
        slot_ids,
    }
}

/// Validates a set of slots against section facts (L4-D-020-BH-004/005).
pub fn validate(slots: &[SlotFacts], sections: &[SectionFacts]) -> Vec<Conflict> {
    let mut out = Vec::new();
    if slots.is_empty() {
        out.push(critical(
            "empty_timetable",
            "the timetable has no slots".into(),
            vec![],
        ));
    }
    for (i, a) in slots.iter().enumerate() {
        for b in &slots[i + 1..] {
            if !overlaps(a, b) {
                continue;
            }
            if a.section_id == b.section_id {
                out.push(critical(
                    "section_overlap",
                    format!("section {} has two overlapping slots", a.section_label),
                    vec![a.id, b.id],
                ));
            }
            if let Some(f) = a.faculty.iter().find(|f| b.faculty.contains(f)) {
                out.push(critical(
                    "faculty_overlap",
                    format!(
                        "{f} teaches {} and {} at the same time",
                        a.section_label, b.section_label
                    ),
                    vec![a.id, b.id],
                ));
            }
            if let (Some(ra), Some(rb)) = (&a.room_label, &b.room_label) {
                if !ra.trim().is_empty() && ra.trim().eq_ignore_ascii_case(rb.trim()) {
                    out.push(critical(
                        "room_overlap",
                        format!("room {} is double-booked", ra.trim()),
                        vec![a.id, b.id],
                    ));
                }
            }
        }
    }
    for s in sections {
        let mine: Vec<Uuid> = slots
            .iter()
            .filter(|x| x.section_id == s.section_id)
            .map(|x| x.id)
            .collect();
        if !s.schedulable && !mine.is_empty() {
            out.push(critical(
                "section_not_schedulable",
                format!(
                    "section {} is no longer offered and cannot be scheduled",
                    s.label
                ),
                mine.clone(),
            ));
        }
        if let Some(n) = s.sessions_per_week {
            if s.schedulable && (mine.len() as i16) < n {
                out.push(Conflict {
                    code: "requirement_shortfall".into(),
                    severity: "warning".into(),
                    message: format!(
                        "section {} needs {n} sessions a week, {} scheduled",
                        s.label,
                        mine.len()
                    ),
                    slot_ids: mine,
                });
            }
        }
    }
    out
}

/// `true` when no critical finding remains.
pub fn is_valid(conflicts: &[Conflict]) -> bool {
    conflicts.iter().all(|c| c.severity != "critical")
}

/// A generated slot (no id yet).
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedSlot {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub day_of_week: i16,
    pub starts_at: NaiveTime,
    pub ends_at: NaiveTime,
    pub room_label: Option<String>,
}

/// Teaching day bounds of the generator.
const DAY_START_MIN: u32 = 9 * 60;
const DAY_END_MIN: u32 = 17 * 60;

fn t(min: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(min / 60, min % 60, 0).unwrap_or(NaiveTime::MIN)
}

/// Greedy draft generation (L4-D-020-BH-003): sections with a requirement get
/// `sessions_per_week` slots between 09:00 and 17:00 on their preferred days (default Mon–Fri),
/// one per day where possible, never overlapping the same section, faculty member or room.
/// Sections that cannot be placed fully keep fewer slots; validation reports the shortfall.
pub fn generate(sections: &[SectionFacts]) -> Vec<GeneratedSlot> {
    let mut placed: Vec<SlotFacts> = Vec::new();
    let mut out = Vec::new();
    let mut ordered: Vec<&SectionFacts> = sections
        .iter()
        .filter(|s| s.schedulable && s.sessions_per_week.is_some())
        .collect();
    ordered.sort_by(|a, b| a.label.cmp(&b.label));
    for s in ordered {
        let need = s.sessions_per_week.unwrap_or(0).max(0) as usize;
        let len = u32::try_from(s.session_minutes.max(15)).unwrap_or(60);
        let days: Vec<i16> = if s.preferred_days.is_empty() {
            vec![1, 2, 3, 4, 5]
        } else {
            s.preferred_days.clone()
        };
        let mut got = 0usize;
        // pass 0: at most one slot per day; pass 1: allow a second slot on a day
        for pass in 0..2 {
            for &day in &days {
                if got >= need {
                    break;
                }
                let same_day = placed
                    .iter()
                    .filter(|p| p.section_id == s.section_id && p.day_of_week == day)
                    .count();
                if same_day > pass {
                    continue;
                }
                let mut start = DAY_START_MIN;
                while start + len <= DAY_END_MIN {
                    let cand = SlotFacts {
                        id: Uuid::nil(),
                        section_id: s.section_id,
                        section_label: s.label.clone(),
                        day_of_week: day,
                        starts_at: t(start),
                        ends_at: t(start + len),
                        room_label: s.room_label.clone(),
                        faculty: s.faculty.clone(),
                    };
                    let probe = [placed.as_slice(), std::slice::from_ref(&cand)].concat();
                    let clash = validate(&probe, &[])
                        .iter()
                        .any(|c| c.severity == "critical" && c.code != "empty_timetable");
                    if !clash {
                        out.push(GeneratedSlot {
                            section_id: s.section_id,
                            offering_id: s.offering_id,
                            day_of_week: day,
                            starts_at: cand.starts_at,
                            ends_at: cand.ends_at,
                            room_label: s.room_label.clone(),
                        });
                        placed.push(cand);
                        got += 1;
                        break;
                    }
                    start += 60;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use TimetableStatus::*;

    fn slot(section: u128, day: i16, from: u32, to: u32, room: &str, fac: &[&str]) -> SlotFacts {
        SlotFacts {
            id: Uuid::from_u128(section * 1000 + u128::from(from)),
            section_id: Uuid::from_u128(section),
            section_label: format!("S{section}"),
            day_of_week: day,
            starts_at: t(from * 60),
            ends_at: t(to * 60),
            room_label: Some(room.into()),
            faculty: fac.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sec(id: u128, n: i16, fac: &[&str], room: &str) -> SectionFacts {
        SectionFacts {
            section_id: Uuid::from_u128(id),
            offering_id: Uuid::from_u128(id + 100),
            label: format!("S{id}"),
            schedulable: true,
            sessions_per_week: Some(n),
            session_minutes: 60,
            preferred_days: vec![],
            room_label: Some(room.into()),
            faculty: fac.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn lifecycle() {
        assert!(can_transition(Planning, Draft));
        assert!(can_transition(Draft, Validated));
        assert!(can_transition(Validated, Approved));
        assert!(can_transition(Approved, Published));
        assert!(can_transition(Published, Active));
        assert!(can_transition(Active, Revised));
        assert!(can_transition(Active, Retired));
        assert!(can_transition(Retired, Archived));
        assert!(
            !can_transition(Draft, Published),
            "publication needs validation and approval"
        );
        assert!(
            !can_transition(Archived, Active),
            "archived never re-enters operation"
        );
        assert!(!can_transition(Retired, Active));
        assert!(
            !can_transition(Published, Draft),
            "published versions are immutable"
        );
        assert!(slots_editable(Draft) && !slots_editable(Published));
    }

    #[test]
    fn conflicts_are_detected() {
        let a = slot(1, 1, 9, 10, "R1", &["meera"]);
        let b = slot(2, 1, 9, 10, "r1", &["meera"]);
        let c = slot(1, 1, 9, 11, "R2", &[]);
        let found = validate(&[a.clone(), b, c], &[]);
        let codes: Vec<&str> = found.iter().map(|c| c.code.as_str()).collect();
        assert!(codes.contains(&"faculty_overlap"));
        assert!(codes.contains(&"room_overlap"));
        assert!(codes.contains(&"section_overlap"));
        assert!(!is_valid(&found));
        // back-to-back is fine
        let d = slot(2, 1, 10, 11, "R1", &["meera"]);
        assert!(is_valid(&validate(&[a, d], &[])));
        assert!(
            !is_valid(&validate(&[], &[])),
            "empty timetable is not publishable"
        );
    }

    #[test]
    fn shortfall_is_a_warning_and_unschedulable_is_critical() {
        let a = slot(1, 1, 9, 10, "R1", &[]);
        let mut s = sec(1, 3, &[], "R1");
        let found = validate(std::slice::from_ref(&a), std::slice::from_ref(&s));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, "warning");
        assert!(is_valid(&found));
        s.schedulable = false;
        assert!(!is_valid(&validate(&[a], &[s])));
    }

    #[test]
    fn generator_places_sessions_without_conflicts() {
        let sections = vec![
            sec(1, 3, &["meera"], "R1"),
            sec(2, 3, &["meera"], "R1"),
            sec(3, 2, &["ravi"], "R2"),
        ];
        let slots = generate(&sections);
        assert_eq!(slots.len(), 8);
        let facts: Vec<SlotFacts> = slots
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let s = sections
                    .iter()
                    .find(|s| s.section_id == g.section_id)
                    .unwrap();
                SlotFacts {
                    id: Uuid::from_u128(i as u128 + 1),
                    section_id: g.section_id,
                    section_label: s.label.clone(),
                    day_of_week: g.day_of_week,
                    starts_at: g.starts_at,
                    ends_at: g.ends_at,
                    room_label: g.room_label.clone(),
                    faculty: s.faculty.clone(),
                }
            })
            .collect();
        let found = validate(&facts, &sections);
        assert!(found.is_empty(), "{found:?}");
        // one slot per day per section on the first pass
        let s1: Vec<i16> = slots
            .iter()
            .filter(|g| g.section_id == Uuid::from_u128(1))
            .map(|g| g.day_of_week)
            .collect();
        assert_eq!(s1, vec![1, 2, 3]);
    }

    #[test]
    fn generator_respects_preferred_days_and_skips_unschedulable() {
        let mut a = sec(1, 2, &[], "R1");
        a.preferred_days = vec![2, 4];
        let mut b = sec(2, 2, &[], "R9");
        b.schedulable = false;
        let slots = generate(&[a, b]);
        assert_eq!(slots.len(), 2);
        assert!(slots
            .iter()
            .all(|g| g.day_of_week == 2 || g.day_of_week == 4));
    }
}
