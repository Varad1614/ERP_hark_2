-- ERP_EXTENSIONS 1A — bank statement files (CSV / OFX / MT940) on top of 5C's JSON-lines import.
-- Logic ported from HeyPuter/everything accounting bankimport.js (MIT); see THIRD_PARTY_NOTICES.md.

ALTER TABLE finance.bank_statement
    ADD COLUMN source_format   text    NOT NULL DEFAULT 'jsonl'
                                       CHECK (source_format IN ('jsonl', 'csv', 'ofx', 'mt940')),
    -- Lines in the file that an earlier statement of the same account already holds (not stored).
    ADD COLUMN duplicate_lines integer NOT NULL DEFAULT 0 CHECK (duplicate_lines >= 0);

-- The bank's own transaction id (OFX FITID / MT940 bank reference), for re-import de-duplication.
ALTER TABLE finance.bank_statement_line ADD COLUMN fitid text;

-- De-duplication reads an account's earlier lines by value date.
CREATE INDEX bank_statement_line_value_date_idx
    ON finance.bank_statement_line (bank_statement_id, value_date);
