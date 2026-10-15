-- Part 1 S2 (DEF-C-002): a closed admission is reopened by at most one live readmission.
--
-- `admission_open_per_student` only covers admissions that already have a student; a cancelled,
-- withdrawn or expired admission without a student could be readmitted again and again (each call
-- with a new Idempotency-Key), opening several admissions for one person. The application code
-- refuses that with a 409; this index makes the database refuse it too, also under concurrency.
CREATE UNIQUE INDEX admission_one_live_readmission
    ON student.admission (previous_admission_id)
    WHERE previous_admission_id IS NOT NULL AND status IN ('initiated', 'documents_submitted',
        'verification_completed', 'approval_pending', 'approved', 'financially_cleared',
        'confirmed', 'student_created');
