-- Part 1 · DB integrity checks against the demo database (after the journey + restart).
--
-- Run by deploy/ci/db-integrity.sh (workflow "SutraERP Part 1 demo"). Every check inserts one row
-- (category, check, violations, detail) into a temp table; the wrapper fails the job when any
-- violation is > 0. Everything runs in ONE transaction that is ROLLED BACK at the end, so the
-- negative probes (writes that the database must refuse) leave nothing behind.
-- Report: docs/readiness/DATABASE_INTEGRITY_REPORT.md.
\set ON_ERROR_STOP 1
BEGIN;
CREATE TEMP TABLE integrity (category text, check_name text, violations bigint, detail text);

-- ------------------------------------------------------------------ 1. migrations
INSERT INTO integrity
SELECT 'migrations', 'all applied migrations succeeded', count(*) FILTER (WHERE NOT success),
       count(*) || ' applied, latest ' || max(version)
FROM _sqlx_migrations;
INSERT INTO integrity
SELECT 'migrations', 'applied in version order', count(*) FILTER (WHERE rn_time <> rn_version), ''
FROM (SELECT version, row_number() OVER (ORDER BY installed_on, version) rn_time,
             row_number() OVER (ORDER BY version) rn_version FROM _sqlx_migrations) m;
INSERT INTO integrity
SELECT 'migrations', 'checksums present', count(*) FILTER (WHERE checksum IS NULL OR length(checksum) = 0), ''
FROM _sqlx_migrations;

-- ------------------------------------------------------------------ 2. referential integrity
-- Every foreign key in the application schemas: invalid (NOT VALID / never validated) constraints
-- and rows whose referenced parent is missing (possible only if a constraint was bypassed).
INSERT INTO integrity
SELECT 'fk', 'foreign keys validated', count(*) FILTER (WHERE NOT convalidated),
       count(*) || ' foreign keys in the application schemas'
FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
WHERE c.contype = 'f' AND n.nspname NOT IN ('pg_catalog', 'information_schema');

DO $$
DECLARE
    r record;
    cond text;
    n bigint;
    total bigint := 0;
    checked int := 0;
    bad text := '';
BEGIN
    FOR r IN
        SELECT c.oid, c.conname, c.conrelid::regclass AS child, c.confrelid::regclass AS parent,
               c.conkey, c.confkey
        FROM pg_constraint c JOIN pg_namespace ns ON ns.oid = c.connamespace
        WHERE c.contype = 'f' AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
    LOOP
        SELECT string_agg(format('ch.%I IS NOT NULL', a.attname), ' AND ')
          INTO cond
          FROM unnest(r.conkey) WITH ORDINALITY k(attnum, i)
          JOIN pg_attribute a ON a.attrelid = r.child AND a.attnum = k.attnum;
        EXECUTE format(
            'SELECT count(*) FROM %s ch WHERE %s AND NOT EXISTS (SELECT 1 FROM %s p WHERE %s)',
            r.child, cond, r.parent,
            (SELECT string_agg(format('p.%I = ch.%I', pa.attname, ca.attname), ' AND ')
               FROM unnest(r.conkey, r.confkey) AS k(ck, pk)
               JOIN pg_attribute ca ON ca.attrelid = r.child AND ca.attnum = k.ck
               JOIN pg_attribute pa ON pa.attrelid = r.parent AND pa.attnum = k.pk))
          INTO n;
        checked := checked + 1;
        IF n > 0 THEN
            total := total + n;
            bad := bad || r.conname || '=' || n || ' ';
        END IF;
    END LOOP;
    INSERT INTO integrity VALUES ('fk', 'no orphan rows behind any foreign key', total,
                                  checked || ' foreign keys scanned ' || bad);
END $$;

-- Cross-module references (module boundaries: plain uuids, no FK) — logical orphans.
INSERT INTO integrity
SELECT 'orphans', 'section enrolments → student', count(*), ''
FROM academic.section_enrolment e WHERE NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = e.student_id);
INSERT INTO integrity
SELECT 'orphans', 'section enrolments → student context', count(*), ''
FROM academic.section_enrolment e WHERE NOT EXISTS (SELECT 1 FROM student.student_context c WHERE c.id = e.student_context_id);
INSERT INTO integrity
SELECT 'orphans', 'attendance records → student', count(*), ''
FROM academic.attendance_record a WHERE NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = a.student_id);
INSERT INTO integrity
SELECT 'orphans', 'student accounts → student', count(*), ''
FROM finance.student_account a WHERE a.student_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = a.student_id);
INSERT INTO integrity
SELECT 'orphans', 'charges → student', count(*), ''
FROM finance.charge c WHERE c.student_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = c.student_id);
INSERT INTO integrity
SELECT 'orphans', 'exam registrations → student', count(*), ''
FROM exams.exam_registration r WHERE NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = r.student_id);
INSERT INTO integrity
SELECT 'orphans', 'term results → student', count(*), ''
FROM exams.term_result r WHERE NOT EXISTS (SELECT 1 FROM student.student s WHERE s.id = r.student_id);
INSERT INTO integrity
SELECT 'orphans', 'student contexts → institution', count(*), ''
FROM student.student_context c WHERE NOT EXISTS (SELECT 1 FROM institution.institution i WHERE i.id = c.institution_id);
INSERT INTO integrity
SELECT 'orphans', 'journal entries → source record (charge / payment / adjustment / allocation)', count(*),
       coalesce(string_agg(DISTINCT j.source_type, ','), '')
FROM finance.journal_entry j
WHERE (j.source_type = 'charge' AND NOT EXISTS (SELECT 1 FROM finance.charge x WHERE x.id = j.source_id))
   OR (j.source_type = 'payment' AND NOT EXISTS (SELECT 1 FROM finance.payment x WHERE x.id = j.source_id))
   OR (j.source_type = 'adjustment' AND NOT EXISTS (SELECT 1 FROM finance.adjustment x WHERE x.id = j.source_id));

-- ------------------------------------------------------------------ 3. balances
INSERT INTO integrity
SELECT 'balance', 'every journal entry: ≥ 2 lines, Σdebit = Σcredit = header totals', count(*), ''
FROM finance.journal_entry j
LEFT JOIN LATERAL (SELECT count(*) n, coalesce(sum(debit), 0) d, coalesce(sum(credit), 0) c
                   FROM finance.journal_line l WHERE l.journal_entry_id = j.id) l ON true
WHERE l.n < 2 OR l.d <> l.c OR l.d <> j.total_debit OR l.c <> j.total_credit;
INSERT INTO integrity
SELECT 'balance', 'ledger balanced per institution (Σdebit = Σcredit)', count(*),
       coalesce(string_agg(institution_id::text, ','), '')
FROM (SELECT j.institution_id, sum(l.debit) d, sum(l.credit) c
      FROM finance.journal_line l JOIN finance.journal_entry j ON j.id = l.journal_entry_id
      GROUP BY j.institution_id HAVING sum(l.debit) <> sum(l.credit)) x;
INSERT INTO integrity
SELECT 'balance', 'charge settled amount = Σ live allocations', count(*), ''
FROM finance.charge c
WHERE c.settled_amount <> coalesce((SELECT sum(a.amount) FROM finance.payment_allocation a
                                    WHERE a.charge_id = c.id AND a.status = 'allocated'), 0);
INSERT INTO integrity
SELECT 'balance', 'charge adjustment amount = Σ posted adjustments (debit note +, others −)', count(*), ''
FROM finance.charge c
WHERE c.adjustment_amount <> coalesce((SELECT sum(CASE WHEN a.kind = 'debit_note' THEN a.amount ELSE -a.amount END)
                                       FROM finance.adjustment a WHERE a.charge_id = c.id AND a.status = 'posted'), 0);
INSERT INTO integrity
SELECT 'balance', 'charge outstanding within [0, total + adjustments]', count(*), ''
FROM finance.charge c WHERE c.outstanding_amount < 0 OR c.outstanding_amount > c.total_amount + greatest(c.adjustment_amount, 0);
INSERT INTO integrity
SELECT 'balance', 'completed payment = Σ live allocations + unapplied', count(*), ''
FROM finance.payment p
WHERE p.status = 'completed'
  AND p.amount <> p.unapplied_amount + coalesce((SELECT sum(a.amount) FROM finance.payment_allocation a
                                                 WHERE a.payment_id = p.id AND a.status = 'allocated'), 0);
INSERT INTO integrity
SELECT 'balance', 'receipt amount = payment amount', count(*), ''
FROM finance.receipt r JOIN finance.payment p ON p.id = r.payment_id WHERE r.amount <> p.amount;
INSERT INTO integrity
SELECT 'balance', 'completed payment has its journal entry', count(*), ''
FROM finance.payment p WHERE p.status = 'completed' AND p.journal_entry_id IS NULL;

-- ------------------------------------------------------------------ 4. duplicates
INSERT INTO integrity
SELECT 'duplicates', 'live admissions per applicant (name + date of birth, per institution) ≤ 1', count(*), ''
FROM (SELECT a.institution_id, lower(btrim(p.given_name)) || ' ' || lower(btrim(coalesce(p.family_name, ''))), p.date_of_birth
      FROM student.admission a JOIN student.admission_application p ON p.id = a.application_id
      WHERE a.status NOT IN ('cancelled', 'withdrawn', 'expired', 'rejected', 'completed')
        AND p.date_of_birth IS NOT NULL
      GROUP BY 1, 2, 3 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one receipt per payment', count(*), ''
FROM (SELECT payment_id FROM finance.receipt GROUP BY payment_id HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one journal entry per (source, posting key)', count(*), ''
FROM (SELECT tenant_id, source_type, source_id, posting_key FROM finance.journal_entry
      GROUP BY 1, 2, 3, 4 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'payments per idempotency key ≤ 1', count(*), ''
FROM (SELECT student_account_id, idempotency_key FROM finance.payment WHERE idempotency_key IS NOT NULL
      GROUP BY 1, 2 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one live enrolment per student and section', count(*), ''
FROM (SELECT section_id, student_id FROM academic.section_enrolment WHERE status = 'enrolled'
      GROUP BY 1, 2 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one attendance record per session and student', count(*), ''
FROM (SELECT session_id, student_id FROM academic.attendance_record GROUP BY 1, 2 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one live exam registration per session and student', count(*), ''
FROM (SELECT session_id, student_id FROM exams.exam_registration
      WHERE status NOT IN ('withdrawn', 'cancelled', 'rejected')
      GROUP BY 1, 2 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one published term result per session and student', count(*), ''
FROM (SELECT session_id, student_id FROM exams.term_result WHERE status = 'published'
      GROUP BY 1, 2 HAVING count(*) > 1) d;
INSERT INTO integrity
SELECT 'duplicates', 'one charge per student and fee structure (not cancelled)', count(*), ''
FROM (SELECT student_id, source_id FROM finance.charge
      WHERE source_type = 'fee_structure' AND status <> 'cancelled' AND student_id IS NOT NULL
      GROUP BY 1, 2 HAVING count(*) > 1) d;

-- ------------------------------------------------------------------ 5. idempotency
INSERT INTO integrity
SELECT 'idempotency', 'completed idempotency records carry the stored response', count(*),
       (SELECT count(*) FROM platform.idempotency_key) || ' records'
FROM platform.idempotency_key WHERE state = 'completed' AND (response_status IS NULL OR completed_at IS NULL);
INSERT INTO integrity
SELECT 'idempotency', 'journey payment key (journey-pay-*): one payment, replay stored', 
       CASE WHEN pay <= 1 AND pay + rec >= 1 THEN 0 ELSE 1 END,
       pay || ' payment(s), ' || rec || ' idempotency record(s)'
FROM (SELECT (SELECT count(*) FROM finance.payment WHERE idempotency_key LIKE 'journey-pay-%') pay,
             (SELECT count(*) FROM platform.idempotency_key WHERE idempotency_key LIKE 'journey-pay-%') rec) x;

-- ------------------------------------------------------------------ 6. maker-checker (human approval mode)
INSERT INTO integrity
SELECT 'approvals', 'no auto-approval actor in the audit log (human mode)', count(*), ''
FROM platform.audit_log WHERE actor_id = 'system:auto-approval';
INSERT INTO integrity
SELECT 'approvals', 'admission approved by someone other than its requester', count(*), ''
FROM student.admission_status_history ap
WHERE ap.to_status = 'approved'
  AND EXISTS (SELECT 1 FROM student.admission_status_history rq
              WHERE rq.admission_id = ap.admission_id AND rq.to_status = 'approval_pending' AND rq.actor = ap.actor);
INSERT INTO integrity
SELECT 'approvals', 'posted adjustments decided by someone other than their creator', count(*), ''
FROM finance.adjustment WHERE status = 'posted' AND decided_by IS NOT DISTINCT FROM created_by;
INSERT INTO integrity
SELECT 'approvals', 'publication authorised by someone other than its requester', count(*), ''
FROM exams.publication p
WHERE p.authorisation_requested_by IS NOT NULL
  AND EXISTS (SELECT 1 FROM platform.audit_log l WHERE l.target_id = p.id::text AND l.action = 'exams.publication.decide'
              AND l.actor_id = p.authorisation_requested_by);
INSERT INTO integrity
SELECT 'approvals', 'applied attendance corrections decided by someone other than the requester', count(*), ''
FROM academic.attendance_correction WHERE status = 'applied' AND decided_by IS NOT DISTINCT FROM requested_by;

-- ------------------------------------------------------------------ 7. guards present (Part 1 + ledger + history)
INSERT INTO integrity
SELECT 'guards', 'expected triggers present and enabled', count(*) FILTER (WHERE t.oid IS NULL OR t.tgenabled = 'D'),
       coalesce(string_agg(e.name, ',') FILTER (WHERE t.oid IS NULL OR t.tgenabled = 'D'), '') ||
       ' (' || count(*) || ' expected)'
FROM (VALUES ('finance.journal_entry', 'journal_entry_balanced'), ('finance.journal_line', 'journal_line_balanced'),
             ('finance.journal_entry', 'journal_entry_append_only'), ('finance.journal_line', 'journal_line_append_only'),
             ('academic.attendance_record', 'attendance_record_finalised_guard'),
             ('academic.attendance_audit', 'attendance_audit_immutable'),
             ('student.admission_status_history', 'admission_status_history_immutable'),
             ('exams.publication_snapshot', 'publication_snapshot_append_only'),
             ('exams.paper_mark_history', 'paper_mark_history_append_only'),
             ('platform.audit_log', 'audit_log_no_update_delete')) e(tbl, name)
LEFT JOIN pg_trigger t ON t.tgrelid = e.tbl::regclass AND t.tgname = e.name;
INSERT INTO integrity
SELECT 'guards', 'Part 1 unique indexes present and valid', 2 - count(*) FILTER (WHERE i.indisvalid AND i.indisunique),
       string_agg(c.relname, ',')
FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid
WHERE c.relname IN ('admission_one_live_readmission', 'admission_open_per_student');
INSERT INTO integrity
SELECT 'guards', 'attendance correction maker-checker constraint present', 1 - count(*), ''
FROM pg_constraint WHERE conname = 'attendance_correction_maker_checker';

-- ------------------------------------------------------------------ 8. negative probes (must be refused)
CREATE TEMP TABLE probe_result (name text, refused boolean, msg text);
DO $$
DECLARE
    je finance.journal_entry%ROWTYPE;
BEGIN
    -- append-only ledger
    BEGIN
        UPDATE finance.journal_entry SET description = description || ' (tampered)'
         WHERE id = (SELECT id FROM finance.journal_entry ORDER BY created_at LIMIT 1);
        IF NOT FOUND THEN INSERT INTO probe_result VALUES ('journal_entry UPDATE', false, 'no row to probe');
        ELSE INSERT INTO probe_result VALUES ('journal_entry UPDATE', false, 'update succeeded'); END IF;
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('journal_entry UPDATE', true, SQLERRM);
    END;
    BEGIN
        DELETE FROM finance.journal_line WHERE id = (SELECT id FROM finance.journal_line ORDER BY created_at LIMIT 1);
        INSERT INTO probe_result VALUES ('journal_line DELETE', false, CASE WHEN FOUND THEN 'delete succeeded' ELSE 'no row to probe' END);
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('journal_line DELETE', true, SQLERRM);
    END;
    -- double entry at commit: a header without lines (checked immediately here)
    BEGIN
        SET CONSTRAINTS ALL IMMEDIATE;
        SELECT * INTO je FROM finance.journal_entry WHERE kind = 'standard' ORDER BY created_at LIMIT 1;
        IF je.id IS NULL THEN
            INSERT INTO probe_result VALUES ('unbalanced journal entry INSERT', false, 'no row to copy');
        ELSE
            je.id := gen_random_uuid();
            je.entry_number := 'INTEGRITY-PROBE';
            je.posting_key := 'integrity-probe';
            INSERT INTO finance.journal_entry SELECT je.*;
            INSERT INTO probe_result VALUES ('unbalanced journal entry INSERT', false, 'insert succeeded');
        END IF;
    EXCEPTION WHEN others THEN
        INSERT INTO probe_result VALUES ('unbalanced journal entry INSERT', SQLERRM LIKE '%unbalanced%', SQLERRM);
    END;
    -- append-only status history + audit log
    BEGIN
        UPDATE student.admission_status_history SET reason = 'tampered'
         WHERE id = (SELECT id FROM student.admission_status_history LIMIT 1);
        INSERT INTO probe_result VALUES ('admission_status_history UPDATE', false, CASE WHEN FOUND THEN 'update succeeded' ELSE 'no row to probe' END);
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('admission_status_history UPDATE', true, SQLERRM);
    END;
    BEGIN
        DELETE FROM platform.audit_log WHERE id = (SELECT id FROM platform.audit_log LIMIT 1);
        INSERT INTO probe_result VALUES ('audit_log DELETE', false, CASE WHEN FOUND THEN 'delete succeeded' ELSE 'no row to probe' END);
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('audit_log DELETE', true, SQLERRM);
    END;
    BEGIN
        UPDATE exams.paper_mark_history SET paper_mark_id = paper_mark_id
         WHERE id = (SELECT id FROM exams.paper_mark_history LIMIT 1);
        INSERT INTO probe_result VALUES ('paper_mark_history UPDATE', false, CASE WHEN FOUND THEN 'update succeeded' ELSE 'no row to probe' END);
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('paper_mark_history UPDATE', true, SQLERRM);
    END;
    -- Part 1 (DEF-D-009): a finalised register cannot be edited outside a governed correction
    BEGIN
        UPDATE academic.attendance_record r
           SET mark = CASE WHEN r.mark = 'present' THEN 'absent' ELSE 'present' END
         WHERE r.id = (SELECT a.id FROM academic.attendance_record a
                       JOIN academic.teaching_session s ON s.id = a.session_id
                       WHERE s.attendance_status = 'finalised' LIMIT 1);
        INSERT INTO probe_result VALUES ('finalised attendance UPDATE', false, CASE WHEN FOUND THEN 'update succeeded' ELSE 'no finalised record to probe' END);
    EXCEPTION WHEN others THEN INSERT INTO probe_result VALUES ('finalised attendance UPDATE', true, SQLERRM);
    END;
END $$;
INSERT INTO integrity
SELECT 'probes', 'refused: ' || name, CASE WHEN refused THEN 0 ELSE 1 END, left(msg, 160) FROM probe_result;

-- ------------------------------------------------------------------ inventory (informational)
INSERT INTO integrity
SELECT 'inventory', 'triggers in application schemas', 0, count(*)::text
FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE NOT t.tgisinternal AND n.nspname NOT IN ('pg_catalog', 'information_schema');
INSERT INTO integrity
SELECT 'inventory', 'check / unique / exclusion constraints', 0,
       count(*) FILTER (WHERE contype = 'c') || ' check, ' || count(*) FILTER (WHERE contype = 'u') || ' unique, ' ||
       count(*) FILTER (WHERE contype = 'x') || ' exclusion'
FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
WHERE n.nspname NOT IN ('pg_catalog', 'information_schema');
INSERT INTO integrity
SELECT 'inventory', 'partial unique indexes', 0, count(*)::text
FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE i.indisunique AND i.indpred IS NOT NULL AND n.nspname NOT IN ('pg_catalog', 'information_schema');

\pset format aligned
\pset footer off
SELECT category, check_name, violations, detail FROM integrity ORDER BY category = 'inventory', category, check_name;
\pset format unaligned
\pset tuples_only on
SELECT 'INTEGRITY_VIOLATIONS=' || coalesce(sum(violations), 0) FROM integrity;
ROLLBACK;
