//! Duplicate-applicant detection (Part 1 S2: DEF-C-003 at application time, DEF-C-002 on
//! readmission).
//!
//! L2-C01 requires that intake "shall never duplicate an existing institutional Student
//! identity". The person key is the normalised name (given + family, case- and
//! whitespace-insensitive) + date of birth + at least one matching contact (e-mail,
//! case-insensitive, or the last ten digits of the phone number). Candidates are searched in the
//! institution's in-flight applications (not yet offered, not rejected / withdrawn), its live
//! admissions (not closed, not yet completed) and the tenant's non-archived students that hold a
//! context in the institution. Without a date of birth or any contact there is no key and no
//! detection.

use chrono::NaiveDate;
use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Inputs of the person key (raw; normalised in SQL so both sides use the same rules).
#[derive(Clone, Debug)]
pub struct PersonKey {
    name: String,
    date_of_birth: NaiveDate,
    email: Option<String>,
    phone: Option<String>,
}

impl PersonKey {
    /// `None` when the key is incomplete (no date of birth, or neither e-mail nor phone).
    pub fn new(
        given: &str,
        family: Option<&str>,
        date_of_birth: Option<NaiveDate>,
        email: Option<&str>,
        phone: Option<&str>,
    ) -> Option<Self> {
        let date_of_birth = date_of_birth?;
        let email = email
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_owned);
        // A phone number needs enough digits to identify anyone.
        let phone = phone
            .filter(|p| p.chars().filter(char::is_ascii_digit).count() >= 7)
            .map(str::to_owned);
        if email.is_none() && phone.is_none() {
            return None;
        }
        let name = match family.map(str::trim).filter(|f| !f.is_empty()) {
            Some(f) => format!("{} {f}", given.trim()),
            None => given.trim().to_owned(),
        };
        Some(Self {
            name,
            date_of_birth,
            email,
            phone,
        })
    }
}

/// One record that looks like the same person.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateCandidate {
    /// `application`, `admission` or `student`.
    pub kind: String,
    pub id: Uuid,
}

/// Candidates matching `key` in `institution`, oldest first; `exclude_admission` leaves one
/// admission out (the readmission source).
pub async fn find(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    key: &PersonKey,
    exclude_admission: Option<Uuid>,
) -> Result<Vec<DuplicateCandidate>, KernelError> {
    const NAME: &str = "lower(regexp_replace(btrim(concat_ws(' ', {g}, {f})), '\\s+', ' ', 'g'))";
    let name = |t: &str| {
        NAME.replace("{g}", &format!("{t}.given_name"))
            .replace("{f}", &format!("{t}.family_name"))
    };
    let contact = |email: &str, phone: &str| {
        format!(
            "((k.email IS NOT NULL AND lower(btrim({email})) = k.email) OR (k.phone IS NOT NULL \
             AND right(regexp_replace(coalesce({phone}, ''), '\\D', '', 'g'), 10) = k.phone))"
        )
    };
    let sql = format!(
        "WITH k AS (SELECT lower(regexp_replace(btrim($3::text), '\\s+', ' ', 'g')) AS nm, \
                $4::date AS dob, lower(btrim($5::text)) AS email, \
                CASE WHEN $6::text IS NULL THEN NULL \
                     ELSE right(regexp_replace($6::text, '\\D', '', 'g'), 10) END AS phone) \
         SELECT kind, id FROM ( \
           SELECT 'application'::text AS kind, a.id, a.created_at \
             FROM student.admission_application a, k \
            WHERE a.tenant_id = $1 AND a.institution_id = $2 \
              AND a.status NOT IN ('rejected', 'withdrawn') AND a.admission_id IS NULL \
              AND a.date_of_birth = k.dob AND {an} = k.nm AND {ac} \
           UNION ALL \
           SELECT 'admission'::text, d.id, d.created_at \
             FROM student.admission d, k \
            WHERE d.tenant_id = $1 AND d.institution_id = $2 \
              AND d.status NOT IN ('cancelled', 'withdrawn', 'revoked', 'expired', 'completed') \
              AND ($7::uuid IS NULL OR d.id <> $7) \
              AND d.date_of_birth = k.dob AND {dn} = k.nm AND {dc} \
           UNION ALL \
           SELECT 'student'::text, s.id, s.created_at \
             FROM student.student s, k \
            WHERE s.tenant_id = $1 AND s.status <> 'archived' \
              AND EXISTS (SELECT 1 FROM student.student_context c \
                           WHERE c.student_id = s.id AND c.institution_id = $2) \
              AND s.date_of_birth = k.dob AND {sn} = k.nm AND {sc} \
         ) c ORDER BY created_at, id LIMIT 10",
        an = name("a"),
        ac = contact("a.email", "a.phone"),
        dn = name("d"),
        dc = contact("d.email", "d.phone"),
        sn = name("s"),
        sc = contact("s.primary_email", "s.primary_phone"),
    );
    let rows: Vec<(String, Uuid)> = sqlx::query_as(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(&key.name)
        .bind(key.date_of_birth)
        .bind(key.email.as_deref())
        .bind(key.phone.as_deref())
        .bind(exclude_admission)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(kind, id)| DuplicateCandidate { kind, id })
        .collect())
}

/// The 409 for a detected duplicate; the detail names the first candidate (`kind` + id) so the
/// caller can open it, or resend with an explicit override.
pub fn conflict(candidates: &[DuplicateCandidate], hint: &str) -> KernelError {
    let Some(first) = candidates.first() else {
        return KernelError::Conflict(format!("possible duplicate applicant; {hint}"));
    };
    KernelError::Conflict(format!(
        "possible duplicate applicant: candidate {} {} (name, date of birth and contact match{}); {hint}",
        first.kind,
        first.id,
        if candidates.len() > 1 {
            format!(", {} more candidate(s)", candidates.len() - 1)
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_needs_a_date_of_birth_and_a_contact() {
        let dob = NaiveDate::from_ymd_opt(2007, 3, 14);
        assert!(PersonKey::new("Riya", Some("Kulkarni"), None, Some("r@x.in"), None).is_none());
        assert!(PersonKey::new("Riya", Some("Kulkarni"), dob, None, None).is_none());
        assert!(PersonKey::new("Riya", Some("Kulkarni"), dob, Some(" "), Some("12-34")).is_none());
        let k = PersonKey::new(
            " Riya ",
            Some(" Kulkarni"),
            dob,
            None,
            Some("+91 98765 43210"),
        )
        .unwrap();
        assert_eq!(k.name, "Riya Kulkarni");
        let k = PersonKey::new("Riya", None, dob, Some("r@x.in"), None).unwrap();
        assert_eq!(k.name, "Riya");
    }

    #[test]
    fn the_conflict_names_the_candidate() {
        let id = Uuid::now_v7();
        let e = conflict(
            &[DuplicateCandidate {
                kind: "student".into(),
                id,
            }],
            "override",
        );
        assert!(
            e.to_string().contains(&format!("candidate student {id}")),
            "{e}"
        );
    }
}
