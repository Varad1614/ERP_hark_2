//! **ERP_EXTENSIONS 5B** — shift swap requests, ported from HeyPuter/everything
//! `src/shifts/src/core/swaps.js` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! A request moves one shift from its holder to another staff member. Everything's lifecycle is
//! `pending → approved | declined | cancelled`; SutraERP splits `pending` into `awaiting_peer`
//! (the colleague has not agreed yet) and `awaiting_manager` (agreed, a manager decides), and
//! adds the request kinds `swap` (two shifts change hands), `cover` (anyone may accept) and
//! `claim` (an open shift). Approving applies the request to the roster.

use super::rules::{
    availability_status, find_conflicts, format_hours, net_minutes, num, rule_violations,
    start_of_week, weekly_minutes, Availability, Employee, Rules, Shift,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// `SWAP_STATUS`.
pub const PENDING: &str = "pending";
pub const APPROVED: &str = "approved";
pub const DECLINED: &str = "declined";
pub const CANCELLED: &str = "cancelled";
/// SutraERP's two pending stages.
pub const AWAITING_PEER: &str = "awaiting_peer";
pub const AWAITING_MANAGER: &str = "awaiting_manager";

/// Whether a status is still open (Everything's `pending`).
pub fn is_pending(status: &str) -> bool {
    matches!(status, PENDING | AWAITING_PEER | AWAITING_MANAGER)
}

/// The shift as the request saw it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub date: String,
    pub start: String,
    pub end: String,
    pub role: String,
}

/// A swap request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapRequest {
    pub id: Option<String>,
    pub shift_id: String,
    pub from_employee_id: String,
    pub to_employee_id: String,
    pub reason: String,
    pub status: String,
    pub created_at: Option<String>,
    pub resolved_at: Option<String>,
    pub shift_snapshot: Snapshot,
}

/// `createSwapRequest`: a pending request for `shift` to go to `to`, or why it cannot be asked.
pub fn create_swap_request(
    shift: Option<&Shift>,
    to: Option<&str>,
    reason: &str,
    id: Option<&str>,
    now: Option<&str>,
) -> Result<SwapRequest, String> {
    let shift = shift.ok_or("A shift is required.")?;
    let from = shift
        .assignee_id
        .as_deref()
        .filter(|a| !a.is_empty())
        .ok_or("Only assigned shifts can be swapped.")?;
    let to = to
        .filter(|t| !t.is_empty())
        .ok_or("Choose an employee to take the shift.")?;
    if to == from {
        return Err("The shift is already assigned to that employee.".into());
    }
    Ok(SwapRequest {
        id: id.map(str::to_owned),
        shift_id: shift.id.clone(),
        from_employee_id: from.to_owned(),
        to_employee_id: to.to_owned(),
        reason: reason.trim().to_owned(),
        status: PENDING.into(),
        created_at: now.map(str::to_owned),
        resolved_at: None,
        shift_snapshot: snapshot(shift),
    })
}

/// The snapshot embedded in a request.
pub fn snapshot(shift: &Shift) -> Snapshot {
    Snapshot {
        date: shift.date.clone(),
        start: shift.start.clone(),
        end: shift.end.clone(),
        role: shift.role.clone(),
    }
}

/// `evaluateSwap`: non-blocking checks of giving `shift` to `to`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evaluation {
    pub ok: bool,
    pub warnings: Vec<String>,
}

/// `evaluateSwap`: overlap, availability, the receiver's weekly maximum (when `employees` is
/// given), the working-time limits (when `rules` is) and attendance already recorded for the
/// shift (it would become the receiver's).
pub fn evaluate_swap(
    shift: &Shift,
    to: &str,
    shifts: &[Shift],
    availability: Option<&BTreeMap<String, Availability>>,
    employees: Option<&[Employee]>,
    attendance: Option<&HashSet<String>>,
    rules: Option<&Rules>,
) -> Evaluation {
    let mut warnings = Vec::new();
    if !find_conflicts(shift, shifts, Some(to)).is_empty() {
        warnings.push("The receiving employee already has an overlapping shift.".to_owned());
    }
    match availability_status(shift, availability.and_then(|a| a.get(to))) {
        "unavailable" => {
            warnings.push("The receiving employee is marked unavailable at this time.".to_owned())
        }
        "partial" => warnings.push(
            "The shift only partially fits the receiving employee\u{2019}s availability."
                .to_owned(),
        ),
        _ => {}
    }
    let employee = employees.and_then(|list| list.iter().find(|e| e.id == to));
    let others: Vec<Shift> = shifts
        .iter()
        .filter(|s| s.id != shift.id)
        .cloned()
        .collect();
    if let Some(max) = employee.and_then(|e| e.max_hours) {
        let projected =
            weekly_minutes(&others, to, &start_of_week(&shift.date)) + net_minutes(shift);
        if projected > max * 60.0 {
            warnings.push(format!(
                "This puts {} at {} that week, over their {}h max.",
                employee.map_or("", |e| e.name.as_str()),
                format_hours(projected),
                num(max)
            ));
        }
    }
    if let Some(r) = rules {
        let target = Shift {
            assignee_id: Some(to.to_owned()),
            ..shift.clone()
        };
        let name = employee.map_or("The receiving employee", |e| e.name.as_str());
        for v in rule_violations(&target, to, &others, r, name) {
            warnings.push(v.message);
        }
    }
    if attendance.is_some_and(|a| a.contains(&shift.id)) {
        warnings.push(format!(
            "The attendance already recorded for this shift becomes {}\u{2019}s.",
            employee.map_or("the receiving employee", |e| e.name.as_str())
        ));
    }
    Evaluation {
        ok: warnings.is_empty(),
        warnings,
    }
}

/// `resolveSwap`: a resolved copy; only pending requests resolve.
pub fn resolve_swap(
    swap: &SwapRequest,
    status: &str,
    now: Option<&str>,
) -> Result<SwapRequest, String> {
    if !is_pending(&swap.status) {
        return Err("This request has already been resolved.".into());
    }
    if ![APPROVED, DECLINED, CANCELLED].contains(&status) {
        return Err(format!("Invalid resolution: {status}"));
    }
    Ok(SwapRequest {
        status: status.to_owned(),
        resolved_at: now.map(str::to_owned),
        ..swap.clone()
    })
}

/// `applySwap`: the shift list with the shift reassigned, or why it no longer applies.
pub fn apply_swap(swap: &SwapRequest, shifts: &[Shift]) -> Result<Vec<Shift>, String> {
    let shift = shifts
        .iter()
        .find(|s| s.id == swap.shift_id)
        .ok_or("The shift in this request no longer exists.")?;
    if shift.assignee_id.as_deref() != Some(swap.from_employee_id.as_str()) {
        return Err("The shift has been reassigned since this request was made.".into());
    }
    Ok(shifts
        .iter()
        .map(|s| {
            if s.id == swap.shift_id {
                Shift {
                    assignee_id: Some(swap.to_employee_id.clone()),
                    ..s.clone()
                }
            } else {
                s.clone()
            }
        })
        .collect())
}

/// `pendingSwaps`.
pub fn pending_swaps(swaps: &[SwapRequest]) -> Vec<&SwapRequest> {
    swaps.iter().filter(|s| is_pending(&s.status)).collect()
}

// ------------------------------------------------------------------ SutraERP request kinds

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RosterSwapKind)]
pub enum Kind {
    /// My shift to a named colleague.
    GiveAway,
    /// My shift for one of theirs.
    Swap,
    /// My shift to whoever accepts first (or a named colleague).
    Cover,
    /// An open shift to me.
    Claim,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GiveAway => "give_away",
            Self::Swap => "swap",
            Self::Cover => "cover",
            Self::Claim => "claim",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::GiveAway, Self::Swap, Self::Cover, Self::Claim]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// The status a new request starts in: a claim goes straight to a manager; the rest wait for
/// the colleague (anyone, for a cover without a name).
pub fn initial_status(kind: Kind) -> &'static str {
    match kind {
        Kind::Claim => AWAITING_MANAGER,
        _ => AWAITING_PEER,
    }
}

/// One reassignment: `(shift id, from, to)`.
pub type Move = (String, Option<String>, String);

/// Every reassignment approving a request makes. A swap exchanges two shifts; the rest move one.
pub fn moves(
    kind: Kind,
    shift: &Shift,
    other: Option<&Shift>,
    from: Option<&str>,
    to: &str,
) -> Result<Vec<Move>, String> {
    let holder = shift.assignee_id.as_deref().filter(|a| !a.is_empty());
    if kind == Kind::Claim {
        if holder.is_some() {
            return Err("Somebody already holds this shift.".into());
        }
        return Ok(vec![(shift.id.clone(), None, to.to_owned())]);
    }
    let from = from.ok_or("Only assigned shifts can be swapped.")?;
    if holder != Some(from) {
        return Err("The shift has been reassigned since this request was made.".into());
    }
    if from == to {
        return Err("The shift is already assigned to that employee.".into());
    }
    let mut out = vec![(shift.id.clone(), Some(from.to_owned()), to.to_owned())];
    if kind == Kind::Swap {
        let o = other.ok_or("The shift offered in exchange no longer exists.")?;
        if o.assignee_id.as_deref() != Some(to) {
            return Err(
                "The shift offered in exchange has been reassigned since this request was made."
                    .into(),
            );
        }
        out.push((o.id.clone(), Some(to.to_owned()), from.to_owned()));
    }
    Ok(out)
}

/// Applies `moves` to a working copy of the roster.
pub fn apply_moves(shifts: &[Shift], moves: &[Move]) -> Vec<Shift> {
    shifts
        .iter()
        .map(|s| match moves.iter().find(|m| m.0 == s.id) {
            Some(m) => Shift {
                assignee_id: Some(m.2.clone()),
                ..s.clone()
            },
            None => s.clone(),
        })
        .collect()
}

/// ERP: the receiver's usual duty differs from the shift's (a qualification hint, never a block).
pub fn duty_mismatch(shift: &Shift, receiver: &Employee) -> Option<String> {
    let usual = receiver.role.trim();
    if usual.is_empty() || usual.eq_ignore_ascii_case(shift.role.trim()) {
        return None;
    }
    Some(format!(
        "{}\u{2019}s usual duty is {usual}, not {}.",
        receiver.name,
        shift.role.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift(id: &str, date: &str, who: Option<&str>) -> Shift {
        Shift {
            id: id.into(),
            date: date.into(),
            start: "09:00".into(),
            end: "17:00".into(),
            role: "Floor".into(),
            assignee_id: who.map(str::to_owned),
            ..Shift::default()
        }
    }

    #[test]
    fn requests_evaluations_and_application() {
        let s1 = shift("s1", "2026-08-04", Some("emp1"));
        let w = create_swap_request(Some(&s1), Some("emp2"), " Trip ", Some("w1"), None).unwrap();
        assert_eq!((w.status.as_str(), w.reason.as_str()), (PENDING, "Trip"));
        assert!(create_swap_request(Some(&s1), Some("emp1"), "", None, None).is_err());
        let liam = Employee {
            id: "emp2".into(),
            name: "Liam".into(),
            max_hours: Some(10.0),
            active: true,
            ..Employee::default()
        };
        let existing = shift("s2", "2026-08-03", Some("emp2"));
        let ev = evaluate_swap(
            &s1,
            "emp2",
            &[s1.clone(), existing.clone()],
            None,
            Some(std::slice::from_ref(&liam)),
            None,
            None,
        );
        assert!(ev.warnings[0].contains("over their 10h max"), "{ev:?}");
        let done = resolve_swap(&w, APPROVED, Some("t")).unwrap();
        assert!(resolve_swap(&done, DECLINED, None).is_err());
        let next = apply_swap(&w, std::slice::from_ref(&s1)).unwrap();
        assert_eq!(next[0].assignee_id.as_deref(), Some("emp2"));
        let m = moves(Kind::Swap, &s1, Some(&existing), Some("emp1"), "emp2").unwrap();
        assert_eq!(m.len(), 2);
        let after = apply_moves(&[s1, existing], &m);
        assert_eq!(after[1].assignee_id.as_deref(), Some("emp1"));
        assert_eq!(pending_swaps(&[w]).len(), 1);
    }
}
