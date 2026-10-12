//! Publication lifecycle rules shared by the 2B versioned definitions (pure, no I/O).
//!
//! L2-B04 §2.2: `Draft → Under Review → Approved → Published → Superseded or Retired`. On the
//! wire (`PublicationStatus`): `draft → in_review` (submitted, approval step running),
//! `in_review → published` (approved and publication validation passed) or back to `draft`
//! (returned / rejected — a rejected proposal never becomes published), `published →
//! superseded` (a successor was published) or `retired`. Published content is immutable.

use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::error::KernelError;

/// Versioned definitions with a governed review step (academic structure, curriculum version,
/// school model).
pub fn can_transition(from: PublicationStatus, to: PublicationStatus) -> bool {
    use PublicationStatus as S;
    matches!(
        (from, to),
        (S::Draft, S::InReview)
            | (S::InReview, S::Published | S::Draft)
            | (S::Published, S::Superseded | S::Retired)
    )
}

/// Programme catalogue entries and curriculum containers: no review step of their own
/// (`draft → published → retired`); a curriculum container becomes published with its first
/// published version.
pub fn can_transition_catalogue(from: PublicationStatus, to: PublicationStatus) -> bool {
    use PublicationStatus as S;
    matches!(
        (from, to),
        (S::Draft, S::Published) | (S::Published, S::Retired)
    )
}

fn conflict(what: &str, from: PublicationStatus, to: PublicationStatus) -> KernelError {
    KernelError::Conflict(format!("{what} cannot move from {from} to {to}"))
}

/// 409 unless the governed transition is allowed.
pub fn ensure(
    what: &str,
    from: PublicationStatus,
    to: PublicationStatus,
) -> Result<(), KernelError> {
    if can_transition(from, to) {
        Ok(())
    } else {
        Err(conflict(what, from, to))
    }
}

/// 409 unless the catalogue transition is allowed.
pub fn ensure_catalogue(
    what: &str,
    from: PublicationStatus,
    to: PublicationStatus,
) -> Result<(), KernelError> {
    if can_transition_catalogue(from, to) {
        Ok(())
    } else {
        Err(conflict(what, from, to))
    }
}

/// Content may only be edited while a draft (published versions are immutable).
pub fn ensure_editable(what: &str, status: PublicationStatus) -> Result<(), KernelError> {
    if status == PublicationStatus::Draft {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "{what} is {status}; only a draft can be edited — create a successor version instead"
        )))
    }
}

pub(crate) fn parse_status(raw: &str) -> Result<PublicationStatus, sqlx::Error> {
    PublicationStatus::parse(raw)
        .ok_or_else(|| sqlx::Error::Decode(format!("publication status `{raw}`").into()))
}

/// NHEQF levels are 4.5 … 8 in half steps.
pub fn nheqf_ok(level: f64) -> bool {
    (4.5..=8.0).contains(&level) && crate::curriculum::support::half_credits(level).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use PublicationStatus as S;

    #[test]
    fn governed_matrix() {
        let allowed = [
            (S::Draft, S::InReview),
            (S::InReview, S::Published),
            (S::InReview, S::Draft),
            (S::Published, S::Superseded),
            (S::Published, S::Retired),
        ];
        for from in S::ALL {
            for to in S::ALL {
                assert_eq!(
                    can_transition(*from, *to),
                    allowed.contains(&(*from, *to)),
                    "{from} -> {to}"
                );
            }
        }
        // No shortcuts: draft never publishes without review, retired never comes back.
        assert!(ensure("x", S::Draft, S::Published).is_err());
        assert!(ensure("x", S::Retired, S::Published).is_err());
        assert!(ensure("x", S::Superseded, S::Published).is_err());
    }

    #[test]
    fn catalogue_matrix() {
        for from in S::ALL {
            for to in S::ALL {
                let ok = matches!(
                    (from, to),
                    (S::Draft, S::Published) | (S::Published, S::Retired)
                );
                assert_eq!(can_transition_catalogue(*from, *to), ok);
            }
        }
        assert!(ensure_catalogue("p", S::Retired, S::Published).is_err());
        assert!(ensure_editable("v", S::Published).is_err());
        assert!(ensure_editable("v", S::Draft).is_ok());
        assert!(nheqf_ok(4.5) && nheqf_ok(8.0) && !nheqf_ok(8.5) && !nheqf_ok(5.25));
    }
}
