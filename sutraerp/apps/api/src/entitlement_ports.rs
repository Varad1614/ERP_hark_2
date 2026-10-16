//! Part 2 · S7-entitle: entitlement-aware decorators of the cross-module Finance ports
//! (`docs/readiness/ENTITLEMENT_IMPLEMENTATION.md` §"Modules that read another module's facts").
//!
//! The HTTP gate (`sutra_web::entitlement`) only sees request paths. Admissions, student
//! registration holds, document release, exit clearance and the examination hall-ticket entry
//! condition read Finance-owned facts **through ports**, which the gate never sees. Without these
//! decorators a `core_academic` institution (no `fees` module) would be judged on Finance data it
//! cannot see or settle: dues left from before a downgrade, or charges raised by another module,
//! would block admission confirmation and withhold hall tickets with no way to clear them.
//!
//! * [`EntitledFinanceClearance`] — when `fees` is not active for the tenant, no Finance
//!   obligation applies: admission clearance and student dues answer
//!   [`FinanceEvidence::NotApplicable`] (the same evidence as an institution that has never
//!   raised a charge), so the Domain C / Domain I rules — unchanged — let the student through.
//!   With `fees` active the real reader answers, exactly as before.
//! * [`EntitledExamFees`] — when `fees` is not active, no examination fee is raised in Finance
//!   (`403 entitlement.module_inactive` to the caller of the port): the late-registration fee is
//!   skipped (the registration stands, `raise_late_fee` logs it), so no invisible charge can
//!   withhold a hall ticket later.
//!
//! Decision recorded in the implementation doc: the correct-behaviour fix was preferred over
//! making `fees` a registry dependency of `admissions` / `examinations`, so Core Academic stays a
//! package an institution that collects fees outside SutraERP can buy. The entitlement lookup
//! fails closed (a store error is returned, never treated as "no fees").

use std::sync::Arc;
use sutra_contracts::finance::ports::{ExamFeeCharge, ExamFeePort, ExamFeeRequest};
use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_kernel::entitlement::{module_inactive, EntitlementPolicy};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Module whose facts the decorated ports read / write.
pub const FEES_MODULE: &str = "fees";

/// [`FinanceClearancePort`] that answers `NotApplicable` when `fees` is not active.
pub struct EntitledFinanceClearance {
    inner: Arc<dyn FinanceClearancePort>,
    policy: EntitlementPolicy,
}

impl EntitledFinanceClearance {
    pub fn new(inner: Arc<dyn FinanceClearancePort>, policy: EntitlementPolicy) -> Self {
        Self { inner, policy }
    }

    /// Wraps `inner` (convenience for the composition root).
    pub fn wrap(
        inner: Arc<dyn FinanceClearancePort>,
        policy: &EntitlementPolicy,
    ) -> Arc<dyn FinanceClearancePort> {
        Arc::new(Self::new(inner, policy.clone()))
    }

    async fn fees_active(&self, scope: &ExecutionScope) -> Result<bool, KernelError> {
        self.policy
            .module_active(scope.tenant_id, FEES_MODULE)
            .await
    }
}

#[async_trait::async_trait]
impl FinanceClearancePort for EntitledFinanceClearance {
    async fn admission_clearance(
        &self,
        scope: &ExecutionScope,
        query: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        if !self.fees_active(scope).await? {
            return Ok(FinanceEvidence::NotApplicable);
        }
        self.inner.admission_clearance(scope, query).await
    }

    async fn student_dues(
        &self,
        scope: &ExecutionScope,
        query: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        if !self.fees_active(scope).await? {
            return Ok(FinanceEvidence::NotApplicable);
        }
        self.inner.student_dues(scope, query).await
    }
}

/// [`ExamFeePort`] that raises nothing when `fees` is not active.
pub struct EntitledExamFees {
    inner: Arc<dyn ExamFeePort>,
    policy: EntitlementPolicy,
}

impl EntitledExamFees {
    pub fn new(inner: Arc<dyn ExamFeePort>, policy: EntitlementPolicy) -> Self {
        Self { inner, policy }
    }

    pub fn wrap(inner: Arc<dyn ExamFeePort>, policy: &EntitlementPolicy) -> Arc<dyn ExamFeePort> {
        Arc::new(Self::new(inner, policy.clone()))
    }
}

#[async_trait::async_trait]
impl ExamFeePort for EntitledExamFees {
    async fn raise_exam_fee(
        &self,
        scope: &ExecutionScope,
        request: ExamFeeRequest,
    ) -> Result<ExamFeeCharge, KernelError> {
        if !self
            .policy
            .module_active(scope.tenant_id, FEES_MODULE)
            .await?
        {
            return Err(module_inactive(FEES_MODULE));
        }
        self.inner.raise_exam_fee(scope, request).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use sutra_kernel::entitlement::{
        DefaultEntitlement, EntitlementStore, ManualClock, TenantEntitlement, MODULE_INACTIVE,
    };
    use sutra_kernel::scope::{InstitutionId, TenantId};

    struct Owing(AtomicUsize);

    #[async_trait::async_trait]
    impl FinanceClearancePort for Owing {
        async fn admission_clearance(
            &self,
            _: &ExecutionScope,
            _: AdmissionFinanceQuery,
        ) -> Result<FinanceEvidence, KernelError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(FinanceEvidence::Outstanding {
                detail: "₹1,000.00 outstanding on 1 charge".into(),
            })
        }
        async fn student_dues(
            &self,
            _: &ExecutionScope,
            _: StudentFinanceQuery,
        ) -> Result<FinanceEvidence, KernelError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(FinanceEvidence::Outstanding {
                detail: "₹1,000.00 outstanding on 1 charge".into(),
            })
        }
    }

    struct NeverFees(AtomicUsize);

    #[async_trait::async_trait]
    impl ExamFeePort for NeverFees {
        async fn raise_exam_fee(
            &self,
            _: &ExecutionScope,
            _: ExamFeeRequest,
        ) -> Result<ExamFeeCharge, KernelError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(KernelError::Validation(vec![]))
        }
    }

    #[derive(Default)]
    struct Store(Mutex<Option<TenantEntitlement>>);

    #[async_trait::async_trait]
    impl EntitlementStore for Store {
        async fn load(&self, _: TenantId) -> Result<Option<TenantEntitlement>, KernelError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    struct Broken;

    #[async_trait::async_trait]
    impl EntitlementStore for Broken {
        async fn load(&self, _: TenantId) -> Result<Option<TenantEntitlement>, KernelError> {
            Err(KernelError::Unavailable("database unavailable".into()))
        }
    }

    fn policy(store: Arc<dyn EntitlementStore>, default: &str) -> EntitlementPolicy {
        EntitlementPolicy::new(
            store,
            Arc::new(ManualClock::new(chrono::Utc::now())),
            DefaultEntitlement::Package(default.into()),
        )
    }

    fn scope() -> ExecutionScope {
        ExecutionScope::institution(TenantId::new(), InstitutionId::new())
    }

    fn dues() -> StudentFinanceQuery {
        StudentFinanceQuery {
            institution_id: InstitutionId::new(),
            student_id: uuid::Uuid::now_v7(),
        }
    }

    #[tokio::test]
    async fn without_fees_no_finance_obligation_applies() {
        let inner = Arc::new(Owing(AtomicUsize::new(0)));
        let core = EntitledFinanceClearance::new(
            inner.clone(),
            policy(Arc::new(Store::default()), "core_academic"),
        );
        let s = scope();
        assert_eq!(
            core.student_dues(&s, dues()).await.unwrap(),
            FinanceEvidence::NotApplicable
        );
        let adm = AdmissionFinanceQuery {
            institution_id: InstitutionId::new(),
            admission_id: uuid::Uuid::now_v7(),
            programme_id: uuid::Uuid::now_v7(),
            student_id: None,
        };
        assert_eq!(
            core.admission_clearance(&s, adm.clone()).await.unwrap(),
            FinanceEvidence::NotApplicable
        );
        assert_eq!(inner.0.load(Ordering::SeqCst), 0, "Finance never consulted");

        // With fees active the real evidence passes through unchanged.
        let plus = EntitledFinanceClearance::new(
            inner.clone(),
            policy(Arc::new(Store::default()), "academic_plus"),
        );
        assert!(!plus
            .student_dues(&s, dues())
            .await
            .unwrap()
            .permits_clearance());
        assert!(!plus
            .admission_clearance(&s, adm)
            .await
            .unwrap()
            .permits_clearance());
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn entitlement_lookup_fails_closed() {
        let inner = Arc::new(Owing(AtomicUsize::new(0)));
        let p = EntitledFinanceClearance::new(inner, policy(Arc::new(Broken), "complete_erp"));
        assert!(matches!(
            p.student_dues(&scope(), dues()).await,
            Err(KernelError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn without_fees_no_exam_fee_is_raised() {
        let inner = Arc::new(NeverFees(AtomicUsize::new(0)));
        let core = EntitledExamFees::new(
            inner.clone(),
            policy(Arc::new(Store::default()), "core_academic"),
        );
        let req = ExamFeeRequest {
            institution_id: InstitutionId::new(),
            student_id: uuid::Uuid::now_v7(),
            fee_code: "EXAM-LATE".into(),
            description: "Late examination registration".into(),
            amount: rust_decimal::Decimal::ONE_HUNDRED,
            academic_term_id: None,
            due_date: None,
            source_id: uuid::Uuid::now_v7(),
            actor: None,
        };
        let err = core
            .raise_exam_fee(&scope(), req.clone())
            .await
            .unwrap_err();
        assert_eq!(err.code(), MODULE_INACTIVE);
        assert_eq!(inner.0.load(Ordering::SeqCst), 0, "Finance never called");
        let all = EntitledExamFees::new(
            inner.clone(),
            policy(Arc::new(Store::default()), "complete_erp"),
        );
        assert!(all.raise_exam_fee(&scope(), req).await.is_err());
        assert_eq!(inner.0.load(Ordering::SeqCst), 1, "delegated");
    }
}
