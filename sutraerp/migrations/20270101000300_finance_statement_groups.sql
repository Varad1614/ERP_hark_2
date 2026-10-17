-- ERP_EXTENSIONS 1C — financial statements. Where a ledger account sits on the income &
-- expenditure account, balance sheet and cash flow (Everything accounting's account types, MIT;
-- see THIRD_PARTY_NOTICES.md). NULL = derived from the seeded role / account type.
ALTER TABLE finance.ledger_account ADD COLUMN statement_group text
    CHECK (statement_group IS NULL OR statement_group IN ('bank', 'receivable', 'other_current_asset',
        'fixed_asset', 'other_asset', 'payable', 'credit_card', 'other_current_liability',
        'long_term_liability', 'fund', 'income', 'other_income', 'direct_cost', 'expense', 'other_expense'));
