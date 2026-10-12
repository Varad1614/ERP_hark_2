//! **2B** readiness provider (contract doc §7). Returns **every** key below on each call:
//! `campus.active_campus`, `curriculum.academic_structure_published`,
//! `curriculum.programme_published`, `curriculum.curriculum_published`,
//! `curriculum.school_model_published`.
//!
//! Applicability follows the academic model of the institution's academic structure (the
//! published one, else the newest open draft): programmes and curricula are `not_applicable`
//! for a pure school model, the school model is `not_applicable` unless the model is `school`
//! or `mixed` (or a school model has been drafted).

use crate::shared::readiness::{ReadinessArea, ReadinessItem, ReadinessStatus};
use sqlx::{PgPool, Row};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

/// Counts read live from the 2B tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub campuses: i64,
    pub active_campuses: i64,
    pub structure_published: bool,
    /// `higher_education` / `school` / `mixed` of the governing structure, if any.
    pub model: Option<String>,
    pub programmes: i64,
    pub published_programmes: i64,
    pub published_curriculum_versions: i64,
    pub school_models: i64,
    pub school_model_published: bool,
}

fn status(met: bool) -> ReadinessStatus {
    if met {
        ReadinessStatus::Met
    } else {
        ReadinessStatus::Unmet
    }
}

/// Pure evaluation of the five 2B keys.
pub fn items(f: &Facts) -> Vec<ReadinessItem> {
    let model = f.model.as_deref();
    let higher_ed = model != Some("school");
    let school = matches!(model, Some("school" | "mixed")) || f.school_models > 0;
    let na = ReadinessStatus::NotApplicable;
    vec![
        ReadinessItem::new(
            "campus.active_campus",
            ReadinessArea::Campus,
            "At least one active campus",
            true,
            status(f.active_campuses > 0),
        )
        .detail(format!(
            "{} campus(es) registered, {} active",
            f.campuses, f.active_campuses
        ))
        .link("/campus/campuses"),
        ReadinessItem::new(
            "curriculum.academic_structure_published",
            ReadinessArea::Curriculum,
            "Academic structure published",
            true,
            status(f.structure_published),
        )
        .detail(if f.structure_published {
            "A published academic structure is in force".to_owned()
        } else {
            "No published academic structure yet".to_owned()
        })
        .link("/academics/structure"),
        ReadinessItem::new(
            "curriculum.programme_published",
            ReadinessArea::Curriculum,
            "At least one programme published",
            true,
            if higher_ed {
                status(f.published_programmes > 0)
            } else {
                na
            },
        )
        .detail(if higher_ed {
            format!(
                "{} programme(s) in the catalogue, {} published",
                f.programmes, f.published_programmes
            )
        } else {
            "School academic model: programmes do not apply".to_owned()
        })
        .link("/academics/programmes"),
        ReadinessItem::new(
            "curriculum.curriculum_published",
            ReadinessArea::Curriculum,
            "At least one curriculum version published",
            true,
            if higher_ed {
                status(f.published_curriculum_versions > 0)
            } else {
                na
            },
        )
        .detail(if higher_ed {
            format!(
                "{} published curriculum version(s)",
                f.published_curriculum_versions
            )
        } else {
            "School academic model: curricula do not apply".to_owned()
        })
        .link("/academics/curriculum"),
        ReadinessItem::new(
            "curriculum.school_model_published",
            ReadinessArea::Curriculum,
            "School academic model published",
            true,
            if school {
                status(f.school_model_published)
            } else {
                na
            },
        )
        .detail(if school {
            if f.school_model_published {
                "A published school academic model is in force".to_owned()
            } else {
                "No published school academic model yet".to_owned()
            }
        } else {
            "Not a school institution".to_owned()
        })
        .link("/academics/school-model"),
    ]
}

/// Reads the facts for one institution of the scope's tenant.
pub async fn read_facts(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Facts, KernelError> {
    let r = sqlx::query(
        "SELECT \
           (SELECT count(*) FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2) AS campuses, \
           (SELECT count(*) FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2 AND status = 'active') AS active_campuses, \
           EXISTS (SELECT 1 FROM institution.academic_structure WHERE tenant_id = $1 AND institution_id = $2 AND status = 'published') AS structure_published, \
           (SELECT model FROM institution.academic_structure WHERE tenant_id = $1 AND institution_id = $2 \
              AND status IN ('published', 'draft', 'in_review') \
              ORDER BY (status = 'published') DESC, version_no DESC LIMIT 1) AS model, \
           (SELECT count(*) FROM institution.programme WHERE tenant_id = $1 AND institution_id = $2) AS programmes, \
           (SELECT count(*) FROM institution.programme WHERE tenant_id = $1 AND institution_id = $2 AND status = 'published') AS published_programmes, \
           (SELECT count(*) FROM institution.curriculum_version WHERE tenant_id = $1 AND institution_id = $2 AND status = 'published') AS published_versions, \
           (SELECT count(*) FROM institution.school_academic_model WHERE tenant_id = $1 AND institution_id = $2 AND status <> 'retired') AS school_models, \
           EXISTS (SELECT 1 FROM institution.school_academic_model WHERE tenant_id = $1 AND institution_id = $2 AND status = 'published') AS school_published",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;
    let get = |c: &str| r.try_get::<i64, _>(c).map_err(map_db_error);
    Ok(Facts {
        campuses: get("campuses")?,
        active_campuses: get("active_campuses")?,
        structure_published: r.try_get("structure_published").map_err(map_db_error)?,
        model: r.try_get("model").map_err(map_db_error)?,
        programmes: get("programmes")?,
        published_programmes: get("published_programmes")?,
        published_curriculum_versions: get("published_versions")?,
        school_models: get("school_models")?,
        school_model_published: r.try_get("school_published").map_err(map_db_error)?,
    })
}

pub async fn facts(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Vec<ReadinessItem>, KernelError> {
    Ok(items(&read_facts(pool, scope, institution_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_key<'a>(items: &'a [ReadinessItem], key: &str) -> &'a ReadinessItem {
        items
            .iter()
            .find(|i| i.key == key)
            .unwrap_or_else(|| panic!("{key} missing"))
    }

    #[test]
    fn every_key_every_time_and_applicability() {
        let empty = items(&Facts::default());
        assert_eq!(empty.len(), 5);
        assert_eq!(
            by_key(&empty, "campus.active_campus").status,
            ReadinessStatus::Unmet
        );
        assert_eq!(
            by_key(&empty, "curriculum.programme_published").status,
            ReadinessStatus::Unmet
        );
        assert_eq!(
            by_key(&empty, "curriculum.school_model_published").status,
            ReadinessStatus::NotApplicable
        );
        let school = items(&Facts {
            model: Some("school".into()),
            active_campuses: 1,
            structure_published: true,
            ..Facts::default()
        });
        assert_eq!(
            by_key(&school, "curriculum.programme_published").status,
            ReadinessStatus::NotApplicable
        );
        assert_eq!(
            by_key(&school, "curriculum.curriculum_published").status,
            ReadinessStatus::NotApplicable
        );
        assert_eq!(
            by_key(&school, "curriculum.school_model_published").status,
            ReadinessStatus::Unmet
        );
        let ready = items(&Facts {
            model: Some("mixed".into()),
            campuses: 2,
            active_campuses: 1,
            structure_published: true,
            programmes: 1,
            published_programmes: 1,
            published_curriculum_versions: 1,
            school_models: 1,
            school_model_published: true,
        });
        assert!(ready.iter().all(|i| i.status == ReadinessStatus::Met));
        assert!(ready.iter().all(|i| i.required && i.link.is_some()));
    }
}
