//! Pure rules of slice 8A (contract `docs/batch8-contracts.md` §4.2 slice enums): lifecycle
//! enums and their legal transitions (reservations, work orders, maintenance plans / cycles, labs,
//! practical sessions, equipment), availability windows and free / busy computation, overlap,
//! lab readiness, preventive maintenance periods and the reopen window of resolved tickets.
//! Everything here is side-effect free and unit-tested.

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, Utc};

/// Declares an 8A enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! facility_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

facility_enum!(
    /// Space reservation lifecycle (L2-G10; L3-G-291…293).
    ReservationStatus {
        Requested => "requested",
        Confirmed => "confirmed",
        Rescheduled => "rescheduled",
        Cancelled => "cancelled",
        Completed => "completed",
        Rejected => "rejected",
    }
);

impl ReservationStatus {
    /// Holds the slot (overlap checks, future reservations of a node).
    pub fn holds_slot(self) -> bool {
        matches!(self, Self::Requested | Self::Confirmed | Self::Rescheduled)
    }

    pub fn can_transition(self, to: Self) -> bool {
        use ReservationStatus::*;
        matches!(
            (self, to),
            (Requested, Confirmed)
                | (Requested, Rejected)
                | (Requested, Cancelled)
                | (Confirmed, Rescheduled)
                | (Rescheduled, Rescheduled)
                | (Confirmed | Rescheduled, Cancelled)
                | (Confirmed | Rescheduled, Completed)
        )
    }
}

facility_enum!(
    /// Work order lifecycle (L2-G01; L3-G-020…022, 026).
    WorkOrderStatus {
        Created => "created",
        Assigned => "assigned",
        InProgress => "in_progress",
        OnHold => "on_hold",
        Completed => "completed",
        Verified => "verified",
        Reopened => "reopened",
        Cancelled => "cancelled",
    }
);

impl WorkOrderStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use WorkOrderStatus::*;
        matches!(
            (self, to),
            (Created, Assigned)
                | (Assigned, InProgress)
                | (Assigned, OnHold)
                | (InProgress, OnHold)
                | (OnHold, InProgress)
                | (OnHold, Assigned)
                | (InProgress, Completed)
                | (Completed, Verified)
                | (Completed, Reopened)
                | (Verified, Reopened)
                | (Reopened, Assigned)
                | (Reopened, InProgress)
                | (Created | Assigned | OnHold | Reopened, Cancelled)
        )
    }

    /// Still work to do (not completed / verified / cancelled).
    pub fn is_open(self) -> bool {
        !matches!(self, Self::Completed | Self::Verified | Self::Cancelled)
    }
}

facility_enum!(
    /// Corrective (from a ticket) or preventive (from a plan cycle).
    WorkOrderKind {
        Corrective => "corrective",
        Preventive => "preventive",
    }
);

facility_enum!(
    /// Preventive maintenance plan lifecycle (L3-G-023).
    MaintenancePlanStatus {
        Draft => "draft",
        Active => "active",
        Suspended => "suspended",
        Retired => "retired",
    }
);

impl MaintenancePlanStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use MaintenancePlanStatus::*;
        matches!(
            (self, to),
            (Draft, Active)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Draft | Active | Suspended, Retired)
        )
    }
}

facility_enum!(
    /// Approval of a draft maintenance plan (`MAINTENANCE_PLAN_APPROVAL`).
    PlanApprovalState {
        NotRequested => "not_requested",
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
    }
);

facility_enum!(
    /// One generated cycle of a preventive plan (L3-G-024).
    MaintenanceCycleStatus {
        Scheduled => "scheduled",
        Due => "due",
        Deferred => "deferred",
        Completed => "completed",
        Missed => "missed",
    }
);

impl MaintenanceCycleStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use MaintenanceCycleStatus::*;
        matches!(
            (self, to),
            (Scheduled | Due | Deferred, Deferred)
                | (Scheduled, Due)
                | (Scheduled | Due | Deferred, Completed)
                | (Scheduled | Due | Deferred, Missed)
        )
    }
}

facility_enum!(
    /// How often a preventive plan generates a cycle.
    MaintenanceFrequency {
        Weekly => "weekly",
        Monthly => "monthly",
        Quarterly => "quarterly",
        HalfYearly => "half_yearly",
        Yearly => "yearly",
    }
);

impl MaintenanceFrequency {
    /// Start of the `n`-th period after `start` (`n = 0` is `start`).
    pub fn period_start(self, start: NaiveDate, n: u32) -> Option<NaiveDate> {
        match self {
            Self::Weekly => start.checked_add_days(Days::new(7 * u64::from(n))),
            Self::Monthly => start.checked_add_months(Months::new(n)),
            Self::Quarterly => start.checked_add_months(Months::new(3 * n)),
            Self::HalfYearly => start.checked_add_months(Months::new(6 * n)),
            Self::Yearly => start.checked_add_months(Months::new(12 * n)),
        }
    }
}

facility_enum!(
    /// Laboratory availability (L2-G02; L3-G-050, 060).
    LabStatus {
        NotReady => "not_ready",
        Ready => "ready",
        Restricted => "restricted",
        Unavailable => "unavailable",
    }
);

facility_enum!(
    /// Practical session lifecycle (L3-G-053, 057).
    PracticalSessionStatus {
        Planned => "planned",
        Ready => "ready",
        InProgress => "in_progress",
        Completed => "completed",
        Postponed => "postponed",
        Cancelled => "cancelled",
    }
);

impl PracticalSessionStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use PracticalSessionStatus::*;
        matches!(
            (self, to),
            (Planned, Ready)
                | (Ready, InProgress)
                | (InProgress, Completed)
                | (Planned | Ready, Postponed)
                | (Postponed, Planned)
                | (Postponed, Ready)
                | (Planned | Ready | Postponed, Cancelled)
        )
    }

    /// Students may still be allocated.
    pub fn accepts_allocations(self) -> bool {
        matches!(self, Self::Planned | Self::Ready | Self::Postponed)
    }
}

facility_enum!(
    /// Laboratory equipment status (L3-G-060).
    EquipmentStatus {
        Operational => "operational",
        Restricted => "restricted",
        UnderMaintenance => "under_maintenance",
        OutOfService => "out_of_service",
    }
);

/// Lab status after a readiness check or an equipment change: an incomplete checklist is
/// `not_ready`; any equipment that is not operational makes the lab `restricted`.
pub fn lab_status(checklist_complete: bool, equipment: &[EquipmentStatus]) -> LabStatus {
    if !checklist_complete {
        LabStatus::NotReady
    } else if equipment.iter().any(|e| *e != EquipmentStatus::Operational) {
        LabStatus::Restricted
    } else {
        LabStatus::Ready
    }
}

/// One weekly opening window (ISO weekday 1 = Monday … 7 = Sunday), business-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeeklyWindow {
    pub weekday: u32,
    pub opens: NaiveTime,
    pub closes: NaiveTime,
}

/// The availability profile of a space.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Availability {
    pub bookable: bool,
    /// Empty = open all day every day.
    pub weekly: Vec<WeeklyWindow>,
    pub blackout: Vec<NaiveDate>,
}

/// Opening windows of `date` (`[]` = closed). Without a profile a space is open all day.
pub fn open_windows(
    profile: Option<&Availability>,
    date: NaiveDate,
) -> Vec<(NaiveTime, NaiveTime)> {
    let all_day = (NaiveTime::MIN, end_of_day());
    let Some(p) = profile else {
        return vec![all_day];
    };
    if !p.bookable || p.blackout.contains(&date) {
        return Vec::new();
    }
    if p.weekly.is_empty() {
        return vec![all_day];
    }
    let wd = date.weekday().number_from_monday();
    let mut w: Vec<_> = p
        .weekly
        .iter()
        .filter(|x| x.weekday == wd)
        .map(|x| (x.opens, x.closes))
        .collect();
    w.sort();
    w
}

/// 23:59 — the last bookable minute of a day.
pub fn end_of_day() -> NaiveTime {
    NaiveTime::from_hms_opt(23, 59, 0).unwrap_or(NaiveTime::MIN)
}

/// Why a slot is outside the space's availability (`None` = inside).
pub fn outside_availability(
    profile: Option<&Availability>,
    date: NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
) -> Option<&'static str> {
    if let Some(p) = profile {
        if !p.bookable {
            return Some("the space is not bookable");
        }
        if p.blackout.contains(&date) {
            return Some("the date is a blackout date of the space");
        }
    }
    let windows = open_windows(profile, date);
    if windows.iter().any(|(o, c)| *o <= start && end <= *c) {
        None
    } else {
        Some("the slot is outside the space's opening hours")
    }
}

/// Half-open interval overlap `[a1, a2)` ∩ `[b1, b2)`.
pub fn overlaps(a1: NaiveTime, a2: NaiveTime, b1: NaiveTime, b2: NaiveTime) -> bool {
    a1 < b2 && b1 < a2
}

/// Free windows of a day: the opening windows minus the busy slots.
pub fn free_windows(
    open: &[(NaiveTime, NaiveTime)],
    busy: &[(NaiveTime, NaiveTime)],
) -> Vec<(NaiveTime, NaiveTime)> {
    let mut busy: Vec<_> = busy.to_vec();
    busy.sort();
    let mut out = Vec::new();
    for (o, c) in open {
        let mut cursor = *o;
        for (b1, b2) in &busy {
            if *b2 <= cursor || *b1 >= *c {
                continue;
            }
            if *b1 > cursor {
                out.push((cursor, *b1));
            }
            if *b2 > cursor {
                cursor = *b2;
            }
        }
        if cursor < *c {
            out.push((cursor, *c));
        }
    }
    out
}

/// Period starts of a preventive plan from `from` (inclusive) through `through` (inclusive),
/// bounded by the plan's `end` date; at most `max` periods.
pub fn period_starts(
    start: NaiveDate,
    end: Option<NaiveDate>,
    freq: MaintenanceFrequency,
    from: NaiveDate,
    through: NaiveDate,
    max: usize,
) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut n = 0u32;
    while out.len() < max && n < 2000 {
        let Some(p) = freq.period_start(start, n) else {
            break;
        };
        n += 1;
        if p > through || end.is_some_and(|e| p > e) {
            break;
        }
        if p >= from {
            out.push(p);
        }
    }
    out
}

/// A resolved ticket may be reopened within this many days.
pub const REOPEN_WINDOW_DAYS: i64 = 7;

/// Whether a ticket resolved at `resolved_at` may still be reopened at `now`.
pub fn reopen_allowed(resolved_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    resolved_at.is_some_and(|r| now - r <= chrono::Duration::days(REOPEN_WINDOW_DAYS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN)
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    #[test]
    fn wire_names_roundtrip() {
        for s in ReservationStatus::ALL {
            assert_eq!(ReservationStatus::parse(s.as_str()), Some(*s));
        }
        for s in WorkOrderStatus::ALL {
            assert_eq!(WorkOrderStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(
            MaintenanceFrequency::parse("half_yearly"),
            Some(MaintenanceFrequency::HalfYearly)
        );
        assert_eq!(LabStatus::parse("bogus"), None);
    }

    #[test]
    fn machines() {
        use ReservationStatus as R;
        assert!(R::Requested.can_transition(R::Confirmed));
        assert!(!R::Cancelled.can_transition(R::Confirmed));
        assert!(R::Rescheduled.can_transition(R::Rescheduled));
        use WorkOrderStatus as W;
        assert!(W::Completed.can_transition(W::Verified));
        assert!(!W::Created.can_transition(W::Completed));
        assert!(W::Verified.can_transition(W::Reopened));
        use MaintenancePlanStatus as P;
        assert!(P::Draft.can_transition(P::Active));
        assert!(!P::Retired.can_transition(P::Active));
        use PracticalSessionStatus as S;
        assert!(S::Ready.can_transition(S::InProgress));
        assert!(!S::Planned.can_transition(S::Completed));
        use MaintenanceCycleStatus as C;
        assert!(C::Deferred.can_transition(C::Deferred));
        assert!(!C::Completed.can_transition(C::Deferred));
    }

    #[test]
    fn availability_and_free_busy() {
        let p = Availability {
            bookable: true,
            weekly: vec![WeeklyWindow {
                weekday: 1,
                opens: t(8, 0),
                closes: t(18, 0),
            }],
            blackout: vec![d("2026-12-07")],
        };
        // 2026-11-30 is a Monday.
        assert_eq!(
            outside_availability(Some(&p), d("2026-11-30"), t(9, 0), t(11, 0)),
            None
        );
        assert!(outside_availability(Some(&p), d("2026-11-30"), t(7, 0), t(9, 0)).is_some());
        assert!(outside_availability(Some(&p), d("2026-12-01"), t(9, 0), t(10, 0)).is_some());
        assert!(outside_availability(Some(&p), d("2026-12-07"), t(9, 0), t(10, 0)).is_some());
        assert_eq!(
            outside_availability(None, d("2026-12-01"), t(1, 0), t(2, 0)),
            None
        );
        assert!(overlaps(t(9, 0), t(11, 0), t(10, 0), t(12, 0)));
        assert!(!overlaps(t(9, 0), t(10, 0), t(10, 0), t(12, 0)));
        let open = open_windows(Some(&p), d("2026-11-30"));
        let free = free_windows(&open, &[(t(10, 0), t(11, 0)), (t(9, 0), t(10, 30))]);
        assert_eq!(free, vec![(t(8, 0), t(9, 0)), (t(11, 0), t(18, 0))]);
    }

    #[test]
    fn lab_readiness() {
        use EquipmentStatus::*;
        assert_eq!(lab_status(false, &[]), LabStatus::NotReady);
        assert_eq!(lab_status(true, &[Operational]), LabStatus::Ready);
        assert_eq!(
            lab_status(true, &[Operational, OutOfService]),
            LabStatus::Restricted
        );
    }

    #[test]
    fn preventive_periods() {
        let p = period_starts(
            d("2026-01-31"),
            None,
            MaintenanceFrequency::Monthly,
            d("2026-01-01"),
            d("2026-04-30"),
            10,
        );
        assert_eq!(
            p,
            vec![
                d("2026-01-31"),
                d("2026-02-28"),
                d("2026-03-31"),
                d("2026-04-30")
            ]
        );
        let w = period_starts(
            d("2026-11-02"),
            Some(d("2026-11-20")),
            MaintenanceFrequency::Weekly,
            d("2026-11-03"),
            d("2026-12-31"),
            10,
        );
        assert_eq!(w, vec![d("2026-11-09"), d("2026-11-16")]);
    }

    #[test]
    fn reopen_window() {
        let now = Utc::now();
        assert!(reopen_allowed(Some(now - chrono::Duration::days(2)), now));
        assert!(!reopen_allowed(Some(now - chrono::Duration::days(8)), now));
        assert!(!reopen_allowed(None, now));
    }
}
