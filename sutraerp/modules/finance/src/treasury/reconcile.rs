//! Pure bank reconciliation rules (ERP_EXTENSIONS 1B), ported from HeyPuter/everything
//! `accounting/src/lib/reconcile.js`, `banking.js` (`findMatches`) and `reports.js`
//! (`reconDiscrepancy`) (MIT, see THIRD_PARTY_NOTICES.md), plus the Indian bank
//! reconciliation statement (BRS).
//!
//! A finished reconciliation stores the journal entries it cleared and the amount each had on the
//! bank's ledger account at that moment. "Reconciled" is derived from those records, so finishing
//! never rewrites the journal, and a reconciled entry that changes later (or disappears) shows up
//! as a discrepancy instead of silently moving the reconciled balance.
//!
//! Amounts are integer paise, debit positive on the bank's ledger account, like the original.

use chrono::NaiveDate;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// One posting on the reconciled account, in ledger order (date, entry order, id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub entry: Uuid,
    pub date: NaiveDate,
    /// Debit positive, paise.
    pub amount: i64,
}

/// A reconciliation record of one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recon {
    pub id: Uuid,
    pub account: Uuid,
    pub statement_date: NaiveDate,
    /// Ordering key among reconciliations of the same statement date (finish time).
    pub finished_at: i64,
    /// `true` once finished (`done`); drafts and undone records never count.
    pub done: bool,
    /// Cleared entries, in the order they were recorded.
    pub cleared: Vec<Uuid>,
    /// Amount each cleared entry had on the account when the reconciliation finished.
    pub amounts: HashMap<Uuid, i64>,
}

/// An unreconciled entry on the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub entry: Uuid,
    pub date: NaiveDate,
    pub amount: i64,
    /// Matched to a bank statement line already (the bank has seen it).
    pub cleared: bool,
}

/// Entries reconciled on an account: the union of its finished reconciliations.
pub fn reconciled_ids(recons: &[Recon], account: Uuid) -> HashSet<Uuid> {
    recons
        .iter()
        .filter(|r| r.account == account && r.done)
        .flat_map(|r| r.cleared.iter().copied())
        .collect()
}

/// The latest finished reconciliation (statement date, then finish time; first wins a tie).
pub fn last_reconciliation(recons: &[Recon], account: Uuid) -> Option<&Recon> {
    let mut last: Option<&Recon> = None;
    for r in recons.iter().filter(|r| r.account == account && r.done) {
        let newer = match last {
            None => true,
            Some(l) => {
                r.statement_date > l.statement_date
                    || (r.statement_date == l.statement_date && r.finished_at > l.finished_at)
            }
        };
        if newer {
            last = Some(r);
        }
    }
    last
}

/// Sum of everything reconciled so far (debit positive): each entry once, at the amount the
/// reconciliation recorded, or at its current amount (`current`) when none was recorded.
pub fn beginning_balance(
    recons: &[Recon],
    account: Uuid,
    current: impl Fn(Uuid) -> Option<i64>,
) -> i64 {
    let mut seen = HashSet::new();
    let mut v = 0;
    for r in recons.iter().filter(|r| r.account == account && r.done) {
        for id in &r.cleared {
            if !seen.insert(*id) {
                continue;
            }
            match r.amounts.get(id) {
                Some(a) => v += a,
                None => v += current(*id).unwrap_or(0),
            }
        }
    }
    v
}

/// Amount each entry moves the account (postings summed per entry, first-seen order).
fn per_entry(postings: &[Posting]) -> Vec<(Uuid, NaiveDate, i64)> {
    let mut idx: HashMap<Uuid, usize> = HashMap::new();
    let mut out: Vec<(Uuid, NaiveDate, i64)> = Vec::new();
    for p in postings {
        match idx.get(&p.entry) {
            Some(&i) => out[i].2 += p.amount,
            None => {
                idx.insert(p.entry, out.len());
                out.push((p.entry, p.date, p.amount));
            }
        }
    }
    out
}

/// Unreconciled entries up to the statement date, oldest first (ledger order within a day);
/// entries that net to zero on the account are left out.
pub fn candidates(
    postings: &[Posting],
    reconciled: &HashSet<Uuid>,
    cleared: &HashSet<Uuid>,
    statement_date: Option<NaiveDate>,
) -> Vec<Candidate> {
    let kept: Vec<Posting> = postings
        .iter()
        .filter(|p| !reconciled.contains(&p.entry))
        .filter(|p| statement_date.is_none_or(|d| p.date <= d))
        .cloned()
        .collect();
    let mut out: Vec<Candidate> = per_entry(&kept)
        .into_iter()
        .filter(|(_, _, a)| *a != 0)
        .map(|(entry, date, amount)| Candidate {
            entry,
            date,
            amount,
            cleared: cleared.contains(&entry),
        })
        .collect();
    out.sort_by_key(|c| c.date);
    out
}

/// The numbers at the top of the reconcile screen, in the account's natural sign
/// (`sign` = 1 for a bank, −1 for a card or other credit-normal account).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub beginning: i64,
    pub increases: i64,
    pub decreases: i64,
    pub n_in: i64,
    pub n_out: i64,
    pub cleared: i64,
    pub ending: i64,
    /// Statement ending balance − cleared balance; finishing requires zero.
    pub difference: i64,
}

pub fn summarize(
    sign: i64,
    beginning_debit: i64,
    items: &[Candidate],
    selected: &HashSet<Uuid>,
    ending: i64,
) -> Summary {
    let (mut increases, mut decreases, mut n_in, mut n_out) = (0, 0, 0, 0);
    for c in items.iter().filter(|c| selected.contains(&c.entry)) {
        let natural = c.amount * sign;
        if natural >= 0 {
            increases += natural;
            n_in += 1;
        } else {
            decreases += -natural;
            n_out += 1;
        }
    }
    let beginning = beginning_debit * sign;
    let cleared = beginning + increases - decreases;
    Summary {
        beginning,
        increases,
        decreases,
        n_in,
        n_out,
        cleared,
        ending,
        difference: ending - cleared,
    }
}

/// The amounts a finishing reconciliation records for its selected entries.
pub fn finish_amounts(
    selected: &[Uuid],
    current: impl Fn(Uuid) -> Option<i64>,
) -> HashMap<Uuid, i64> {
    selected
        .iter()
        .filter_map(|id| current(*id).map(|a| (*id, a)))
        .collect()
}

/// A reconciled entry whose amount on the account changed after the reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discrepancy {
    pub reconciliation: Uuid,
    pub account: Uuid,
    pub statement_date: NaiveDate,
    /// `None` when the entry no longer exists.
    pub entry: Option<Uuid>,
    pub was: i64,
    pub now: i64,
    pub change: i64,
}

/// Every finished reconciliation's recorded amounts against the entries' current amounts
/// (`current(account, entry)`: `None` = the entry is gone).
pub fn discrepancies(
    recons: &[Recon],
    current: impl Fn(Uuid, Uuid) -> Option<i64>,
) -> Vec<Discrepancy> {
    let mut out = Vec::new();
    for r in recons.iter().filter(|r| r.done) {
        for id in &r.cleared {
            let Some(&was) = r.amounts.get(id) else {
                continue;
            };
            let now_opt = current(r.account, *id);
            let now = now_opt.unwrap_or(0);
            if was == now {
                continue;
            }
            out.push(Discrepancy {
                reconciliation: r.id,
                account: r.account,
                statement_date: r.statement_date,
                entry: now_opt.map(|_| *id),
                was,
                now,
                change: now - was,
            });
        }
    }
    out
}

/// A statement line to find matches for (amount signed: money in positive).
#[derive(Clone, Debug)]
pub struct MatchLine {
    pub date: NaiveDate,
    pub amount: i64,
}

/// An open receivable (money in) or payable (money out) document.
#[derive(Clone, Debug)]
pub struct OpenDoc {
    pub id: Uuid,
    pub receivable: bool,
    pub date: NaiveDate,
    pub due: Option<NaiveDate>,
    pub voided: bool,
    pub open: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    /// An entry already in the books.
    Entry,
    /// An open document for exactly this amount (probably being paid).
    Open,
}

/// A match suggestion; lower `score` is better (`days × 100` for entries,
/// `10000 + days` for open documents, so any entry beats any document).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub id: Uuid,
    pub score: i64,
}

/// Up to 8 suggestions for a statement line: entries on the account with the same signed amount
/// within `window` days that no other line has taken and no reconciliation has cleared (closest
/// first), then open documents of exactly the line's amount (closest due date first).
pub fn find_matches(
    line: &MatchLine,
    postings: &[Posting],
    window: i64,
    taken: &HashSet<Uuid>,
    reconciled: &HashSet<Uuid>,
    docs: &[OpenDoc],
) -> Vec<Suggestion> {
    let in_window: Vec<Posting> = postings
        .iter()
        .filter(|p| (p.date - line.date).num_days().abs() <= window)
        .cloned()
        .collect();
    let mut out = Vec::new();
    for (entry, date, amount) in per_entry(&in_window) {
        if amount != line.amount || taken.contains(&entry) || reconciled.contains(&entry) {
            continue;
        }
        out.push(Suggestion {
            kind: SuggestionKind::Entry,
            id: entry,
            score: (date - line.date).num_days().abs() * 100,
        });
    }
    let receivable = line.amount > 0;
    for d in docs {
        if d.receivable != receivable || d.voided || d.open != line.amount.abs() {
            continue;
        }
        out.push(Suggestion {
            kind: SuggestionKind::Open,
            id: d.id,
            score: 10_000 + (d.due.unwrap_or(d.date) - line.date).num_days().abs(),
        });
    }
    out.sort_by_key(|s| s.score);
    out.truncate(8);
    out
}

/// Bank reconciliation statement (Indian format) as of a date: the cash book (ledger) balance,
/// adjusted for items only one side has recorded, gives the balance the pass book (bank) should
/// show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Brs {
    /// Balance as per cash book (ledger, debit positive).
    pub book_balance: i64,
    /// Add: cheques issued / payments booked but not yet debited by the bank.
    pub issued_not_presented: i64,
    /// Less: cheques deposited / receipts booked but not yet credited by the bank.
    pub deposited_not_credited: i64,
    /// Add: credits on the bank statement not yet in the books (interest, direct receipts).
    pub bank_credits_not_booked: i64,
    /// Less: debits on the bank statement not yet in the books (charges, direct debits).
    pub bank_debits_not_booked: i64,
    /// Balance the pass book should show.
    pub bank_balance: i64,
    /// Balance the latest statement shows, when one closes on or before the date.
    pub statement_balance: Option<i64>,
    /// `statement_balance − bank_balance` (zero when everything is explained).
    pub difference: Option<i64>,
}

/// `uncleared`: book entries (debit positive) the bank has not shown by the date;
/// `unbooked`: statement lines (money in positive) not matched to the books.
pub fn brs(
    book_balance: i64,
    uncleared: &[i64],
    unbooked: &[i64],
    statement_balance: Option<i64>,
) -> Brs {
    let issued: i64 = uncleared.iter().filter(|a| **a < 0).map(|a| -a).sum();
    let deposited: i64 = uncleared.iter().filter(|a| **a > 0).sum();
    let credits: i64 = unbooked.iter().filter(|a| **a > 0).sum();
    let debits: i64 = unbooked.iter().filter(|a| **a < 0).map(|a| -a).sum();
    let bank_balance = book_balance + issued - deposited + credits - debits;
    Brs {
        book_balance,
        issued_not_presented: issued,
        deposited_not_credited: deposited,
        bank_credits_not_booked: credits,
        bank_debits_not_booked: debits,
        bank_balance,
        statement_balance,
        difference: statement_balance.map(|s| s - bank_balance),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    // Mirrors Everything's tests/reconcile.test.js.
    #[test]
    fn reconcile_lifecycle_matches_the_original_tests() {
        let acc = u(1);
        let (dep, e1, e2) = (u(10), u(11), u(12));
        let postings = vec![
            Posting {
                entry: dep,
                date: d(5, 1),
                amount: 100_000,
            },
            Posting {
                entry: e1,
                date: d(5, 10),
                amount: -2_500,
            },
            Posting {
                entry: e2,
                date: d(6, 2),
                amount: -1_000,
            },
        ];
        let none = HashSet::new();
        let items = candidates(&postings, &none, &none, Some(d(5, 31)));
        assert_eq!(
            items.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![100_000, -2_500]
        );
        let sel: HashSet<Uuid> = [dep, e1].into();
        let s = summarize(1, 0, &items, &sel, 97_500);
        assert_eq!(
            (
                s.beginning,
                s.increases,
                s.decreases,
                s.cleared,
                s.difference
            ),
            (0, 100_000, 2_500, 97_500, 0)
        );
        let partial = summarize(1, 0, &items, &[dep].into(), 97_500);
        assert_eq!(partial.difference, -2_500);

        let current = |id: Uuid| postings.iter().find(|p| p.entry == id).map(|p| p.amount);
        let rec = Recon {
            id: u(99),
            account: acc,
            statement_date: d(5, 31),
            finished_at: 1,
            done: true,
            cleared: vec![dep, e1],
            amounts: finish_amounts(&[dep, e1], current),
        };
        let recons = vec![rec];
        assert_eq!(beginning_balance(&recons, acc, current), 97_500);
        let rec_ids = reconciled_ids(&recons, acc);
        let next = candidates(&postings, &rec_ids, &none, Some(d(6, 30)));
        assert_eq!(
            next.iter().map(|c| c.amount).collect::<Vec<_>>(),
            vec![-1_000]
        );

        // e1 edited to 3000 later: a discrepancy of −500.
        let edited = |_: Uuid, id: Uuid| if id == e1 { Some(-3_000) } else { current(id) };
        let disc = discrepancies(&recons, edited);
        assert_eq!(disc.len(), 1);
        assert_eq!(
            (disc[0].was, disc[0].now, disc[0].change),
            (-2_500, -3_000, -500)
        );
        assert_eq!(last_reconciliation(&recons, acc).map(|r| r.id), Some(u(99)));
    }

    #[test]
    fn matches_prefer_close_entries_over_open_documents() {
        let line = MatchLine {
            date: d(7, 10),
            amount: 5_000,
        };
        let postings = vec![
            Posting {
                entry: u(1),
                date: d(7, 1),
                amount: 5_000,
            },
            Posting {
                entry: u(2),
                date: d(7, 12),
                amount: 5_000,
            },
            Posting {
                entry: u(3),
                date: d(7, 30),
                amount: 5_000,
            },
            Posting {
                entry: u(4),
                date: d(7, 10),
                amount: -5_000,
            },
        ];
        let docs = vec![
            OpenDoc {
                id: u(20),
                receivable: true,
                date: d(6, 1),
                due: Some(d(7, 15)),
                voided: false,
                open: 5_000,
            },
            OpenDoc {
                id: u(21),
                receivable: false,
                date: d(7, 10),
                due: None,
                voided: false,
                open: 5_000,
            },
        ];
        let taken: HashSet<Uuid> = [u(1)].into();
        let m = find_matches(&line, &postings, 10, &taken, &HashSet::new(), &docs);
        assert_eq!(
            m,
            vec![
                Suggestion {
                    kind: SuggestionKind::Entry,
                    id: u(2),
                    score: 200
                },
                Suggestion {
                    kind: SuggestionKind::Open,
                    id: u(20),
                    score: 10_005
                },
            ]
        );
    }

    #[test]
    fn brs_adjusts_book_balance_to_pass_book() {
        // Book 1,00,000; cheque issued 20,000 not presented; deposit 5,000 not credited;
        // bank interest 300 and charges 118 not booked.
        let b = brs(
            100_000_00,
            &[-20_000_00, 5_000_00],
            &[300_00, -118_00],
            Some(115_182_00),
        );
        assert_eq!(
            b.bank_balance,
            100_000_00 + 20_000_00 - 5_000_00 + 300_00 - 118_00
        );
        assert_eq!(b.difference, Some(0));
        assert_eq!(brs(0, &[], &[], None).difference, None);
    }
}
