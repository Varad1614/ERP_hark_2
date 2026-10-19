//! Student establishment from a confirmed admission (L3-C-001 / L4-C-001 BH-001…007) and
//! readmission re-entry (L3-C-005): persona + `pending_activation` context + activation.

use super::support::{next_number, status_changed};
use crate::admission::domain::Admission;
use crate::shared::lifecycle::{change_student_status, StatusChange};
use crate::shared::student::{
    context_from_row, lock_student, student_from_row, StudentRow, CONTEXT_COLUMNS, STUDENT_COLUMNS,
};
use crate::shared::{publish, Actor};
use chrono::{Datelike, NaiveDate};
use sqlx::PgConnection;
use sutra_contracts::student::events::record::{
    StudentCreatedV1, STUDENT_ACTIVATED, STUDENT_ACTIVATED_SCHEMA, STUDENT_CREATED,
    STUDENT_CREATED_SCHEMA,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::status::StudentStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Student number `S<year><5-digit sequence>` (tenant-wide, immutable).
pub fn student_number(year: i32, seq: i64) -> String {
    format!("S{year}{seq:05}")
}

/// Enrolment number `<year>-<5-digit sequence>` (per institution).
pub fn enrolment_number(year: i32, seq: i64) -> String {
    format!("EN{year}{seq:05}")
}

/// Allocates the next enrolment number of an institution.
pub async fn allocate_enrolment_number(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: Uuid,
    today: NaiveDate,
) -> Result<String, KernelError> {
    let seq = next_number(
        conn,
        scope.tenant_id,
        &format!("enrolment:{institution}:{}", today.year()),
    )
    .await?;
    Ok(enrolment_number(today.year(), seq))
}

/// Creates or re-activates the persona and opens the `pending_activation` context for a locked,
/// `confirmed` admission. Emits `student.created` and `student.activated`.
pub async fn establish_from_admission(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    adm: &Admission,
    today: NaiveDate,
    actor: &Actor,
) -> Result<(StudentId, StudentContextId), KernelError> {
    let tenant = scope.tenant_id;
    let change = StatusChange {
        cause_event_type: STUDENT_ACTIVATED,
        reason: Some("admission confirmed"),
        process_instance_id: adm.process_instance_id.as_deref(),
        closed_on: None,
    };
    // 1. Persona: reuse on readmission, else create (never a competing identity).
    let (student, created) = match adm.student_id {
        Some(sid) => {
            let s = lock_student(conn, tenant, sid).await?;
            let s = if matches!(
                s.status,
                StudentStatus::Archived
                    | StudentStatus::QualificationCompleted
                    | StudentStatus::Created
            ) {
                change_student_status(
                    conn,
                    &s,
                    adm.institution_id,
                    StudentStatus::Activated,
                    &change,
                    actor,
                )
                .await?
            } else {
                s
            };
            (s, false)
        }
        None => {
            if let Some(subject) = adm.subject_ref.as_deref() {
                let taken: Option<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM student.student WHERE tenant_id = $1 AND subject_ref = $2 \
                     AND status <> 'archived'",
                )
                .bind(tenant.as_uuid())
                .bind(subject)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db_error)?;
                if taken.is_some() {
                    return Err(KernelError::Conflict(
                        "the applicant's login is already linked to another student; resolve the \
                         duplicate or readmit that student"
                            .into(),
                    ));
                }
            }
            let seq = next_number(conn, tenant, &format!("student:{}", today.year())).await?;
            let sql = format!(
                "INSERT INTO student.student (tenant_id, student_number, given_name, middle_name, \
                 family_name, date_of_birth, gender, primary_email, primary_phone, subject_ref, \
                 origin_admission_id, status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'created') \
                 RETURNING {STUDENT_COLUMNS}"
            );
            let row = sqlx::query(&sql)
                .bind(tenant.as_uuid())
                .bind(student_number(today.year(), seq))
                .bind(&adm.given_name)
                .bind(&adm.middle_name)
                .bind(&adm.family_name)
                .bind(adm.date_of_birth)
                .bind(&adm.gender)
                .bind(&adm.email)
                .bind(&adm.phone)
                .bind(&adm.subject_ref)
                .bind(adm.id.as_uuid())
                .fetch_one(&mut *conn)
                .await
                .map_err(map_db_error)?;
            let s: StudentRow = student_from_row(&row).map_err(map_db_error)?;
            sqlx::query(
                "INSERT INTO student.communication_preference (student_id, tenant_id, \
                 email_opt_in, sms_opt_in) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(s.id.as_uuid())
            .bind(tenant.as_uuid())
            .bind(s.primary_email.is_some())
            .bind(s.primary_phone.is_some())
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let s = change_student_status(
                conn,
                &s,
                adm.institution_id,
                StudentStatus::Activated,
                &change,
                actor,
            )
            .await?;
            (s, true)
        }
    };
    // 2. Context (pending_activation); readmission links the latest closed context here.
    let predecessor: Option<Uuid> = if created {
        None
    } else {
        sqlx::query_scalar(
            "SELECT id FROM student.student_context WHERE tenant_id = $1 AND student_id = $2 \
             AND institution_id = $3 ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(tenant.as_uuid())
        .bind(student.id.as_uuid())
        .bind(adm.institution_id.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
    };
    let sql = format!(
        "INSERT INTO student.student_context (tenant_id, student_id, institution_id, campus_id, \
         programme_id, curriculum_version_id, admission_term_id, admission_year_id, \
         current_term_no, enrolment_type, admission_id, predecessor_context_id, status) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,1,$9,$10,$11,'pending_activation') RETURNING {CONTEXT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(student.id.as_uuid())
        .bind(adm.institution_id.as_uuid())
        .bind(adm.campus_id)
        .bind(adm.programme_id)
        .bind(adm.curriculum_version_id)
        .bind(adm.admission_term_id)
        .bind(adm.admission_year_id)
        .bind(&adm.enrolment_type)
        .bind(adm.id.as_uuid())
        .bind(predecessor)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("student_context_one_live") => {
                KernelError::Conflict(
                    "the student already has a live context in this institution".into(),
                )
            }
            _ => map_db_error(e),
        })?;
    let ctx = context_from_row(&row).map_err(map_db_error)?;
    // 3. Verified admission documents move onto the student's record (Part 2 · S4-docs: with
    //    their stored file, so the student can retrieve it).
    sqlx::query(
        "INSERT INTO student.student_document (tenant_id, student_id, institution_id, \
         document_type, title, file_ref, status, source, admission_document_id, note, \
         document_object_id) \
         SELECT tenant_id, $2, $3, document_type, title, file_ref, status, 'admission', id, note, \
         document_object_id FROM student.admission_document WHERE admission_id = $1 \
         AND status IN ('verified', 'waived')",
    )
    .bind(adm.id.as_uuid())
    .bind(student.id.as_uuid())
    .bind(adm.institution_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    // 4. Events + audit.
    publish::event(
        conn,
        scope,
        STUDENT_CREATED,
        STUDENT_CREATED_SCHEMA,
        student.id.to_string(),
        &StudentCreatedV1 {
            student_id: student.id.as_uuid(),
            student_context_id: ctx.id.as_uuid(),
            institution_id: adm.institution_id.as_uuid(),
            admission_id: Some(adm.id.as_uuid()),
            student_number: student.student_number.clone(),
            programme_id: Some(adm.programme_id),
            student_subject_ref: student.subject_ref.clone(),
        },
        actor,
    )
    .await?;
    publish::event(
        conn,
        scope,
        STUDENT_ACTIVATED,
        STUDENT_ACTIVATED_SCHEMA,
        ctx.id.to_string(),
        &status_changed(
            ctx.id.as_uuid(),
            adm.institution_id,
            Some(student.id.as_uuid()),
            None,
            ctx.status.as_str(),
            ctx.version.0,
            None,
            adm.process_instance_id.clone(),
            student.subject_ref.clone(),
        ),
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "student.student.create",
        "student",
        student.id.to_string(),
        serde_json::json!({
            "admissionId": adm.id,
            "studentContextId": ctx.id,
            "studentNumber": student.student_number,
            "readmission": !created,
        }),
        actor,
    )
    .await?;
    Ok((student.id, ctx.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_padded() {
        assert_eq!(student_number(2026, 7), "S202600007");
        assert_eq!(enrolment_number(2026, 123), "EN202600123");
    }
}
