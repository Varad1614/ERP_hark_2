-- Part 1 S2 (DEF-D-009, DEF-D-002): finalised attendance is protected by the database, not only
-- by application code.
--
-- 1. Once a session's register is `finalised`, its attendance records cannot be updated, except
--    for a mark change that the same transaction has already written to the append-only
--    `attendance_audit` history for an open (requested / approved) correction of that record.
--    That is exactly what the governed correction path (`apply_correction`) does; any other
--    UPDATE (a direct SQL edit, a capture that bypassed the API checks) is refused.
-- 2. An applied correction must have been decided by someone other than its requester
--    (maker-checker). NOT VALID: rows written before this migration are not re-checked, every
--    new or updated row is.

CREATE OR REPLACE FUNCTION academic.attendance_record_finalised_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    v_status text;
BEGIN
    SELECT attendance_status INTO v_status FROM academic.teaching_session WHERE id = OLD.session_id;
    IF v_status IS DISTINCT FROM 'finalised' THEN
        RETURN NEW;
    END IF;
    IF NEW.session_id IS DISTINCT FROM OLD.session_id
        OR NEW.student_id IS DISTINCT FROM OLD.student_id
        OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
        OR NEW.remarks IS DISTINCT FROM OLD.remarks
        OR NEW.recorded_by IS DISTINCT FROM OLD.recorded_by THEN
        RAISE EXCEPTION 'attendance record % belongs to a finalised register and cannot be edited', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.mark IS DISTINCT FROM OLD.mark AND NOT EXISTS (
        SELECT 1
          FROM academic.attendance_audit a
          JOIN academic.attendance_correction c ON c.id = a.correction_id
         WHERE a.record_id = OLD.id
           AND c.record_id = OLD.id
           AND a.from_mark = OLD.mark
           AND a.to_mark = NEW.mark
           AND a.recorded_at = now()
           AND c.status IN ('requested', 'approved')
    ) THEN
        RAISE EXCEPTION 'attendance record % belongs to a finalised register; change it through a governed correction', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER attendance_record_finalised_guard BEFORE UPDATE ON academic.attendance_record
    FOR EACH ROW EXECUTE FUNCTION academic.attendance_record_finalised_guard();

ALTER TABLE academic.attendance_correction
    ADD CONSTRAINT attendance_correction_maker_checker
    CHECK (status <> 'applied' OR decided_by IS DISTINCT FROM requested_by) NOT VALID;
