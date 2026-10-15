-- Part 1 · Stage 2 (DEF-X-002): who asked for a purchase order's approval, so the approval
-- decision can refuse that same user (separation of duties).
ALTER TABLE resources.purchase_order ADD COLUMN approval_requested_by text;
