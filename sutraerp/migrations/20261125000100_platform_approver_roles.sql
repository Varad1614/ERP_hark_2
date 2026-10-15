-- Part 1 · Stage 2 (DEF-X-002): separate approver (checker) roles.
--
-- The role model gains five checker roles that hold a domain's approve permission without its
-- manage permissions (docs/readiness/APPROVAL_CONTROL_MATRIX.md). Every column that stores an
-- application role name is widened to accept them; the three original roles keep their meaning.

ALTER TABLE platform.notification DROP CONSTRAINT notification_recipient_role_check;
ALTER TABLE platform.notification ADD CONSTRAINT notification_recipient_role_check
    CHECK (recipient_role IN ('administrator', 'faculty', 'student', 'finance_approver',
                              'exam_controller', 'registrar', 'payroll_approver',
                              'procurement_approver'));

ALTER TABLE platform.notice DROP CONSTRAINT notice_audience_roles_check;
ALTER TABLE platform.notice ADD CONSTRAINT notice_audience_roles_check
    CHECK (cardinality(audience_roles) >= 1
           AND audience_roles <@ ARRAY['administrator', 'faculty', 'student', 'finance_approver',
                                       'exam_controller', 'registrar', 'payroll_approver',
                                       'procurement_approver']);

ALTER TABLE services.dashboard_assignment DROP CONSTRAINT dashboard_assignment_role_check;
ALTER TABLE services.dashboard_assignment ADD CONSTRAINT dashboard_assignment_role_check
    CHECK (role IN ('administrator', 'faculty', 'student', 'finance_approver', 'exam_controller',
                    'registrar', 'payroll_approver', 'procurement_approver'));
