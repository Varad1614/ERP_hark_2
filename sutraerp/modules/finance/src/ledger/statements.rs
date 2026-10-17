//! **ERP_EXTENSIONS 1C** — financial statements, pure rules.
//!
//! Income & expenditure (profit and loss), balance sheet, cash flow (indirect method) and the
//! general ledger, computed from journal postings. The statement layout, the account tree with
//! "Total for" subtotals, comparison columns, period buckets, the fiscal-year roll of prior
//! surpluses into the accumulated fund and the cash-flow classification are ported from
//! Everything's `accounting/src/lib/{reports,dates,accounts}.js` (MIT, HeyPuter/everything @
//! 06802a1) and replayed against the originals by `tests/golden_statements.rs`. Amounts are
//! exact decimals; a posting is debit-positive (debit − credit).

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

// ------------------------------------------------------------------------- groups

/// Where an account sits on the statements (Everything's account type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatementGroup {
    /// Cash in hand and bank accounts (the cash of the cash-flow statement).
    Bank,
    Receivable,
    OtherCurrentAsset,
    FixedAsset,
    OtherAsset,
    Payable,
    CreditCard,
    OtherCurrentLiability,
    LongTermLiability,
    /// Corpus, accumulated and earmarked funds (equity).
    Fund,
    Income,
    OtherIncome,
    DirectCost,
    Expense,
    OtherExpense,
}

impl StatementGroup {
    pub const ALL: [StatementGroup; 15] = [
        Self::Bank,
        Self::Receivable,
        Self::OtherCurrentAsset,
        Self::FixedAsset,
        Self::OtherAsset,
        Self::Payable,
        Self::CreditCard,
        Self::OtherCurrentLiability,
        Self::LongTermLiability,
        Self::Fund,
        Self::Income,
        Self::OtherIncome,
        Self::DirectCost,
        Self::Expense,
        Self::OtherExpense,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bank => "bank",
            Self::Receivable => "receivable",
            Self::OtherCurrentAsset => "other_current_asset",
            Self::FixedAsset => "fixed_asset",
            Self::OtherAsset => "other_asset",
            Self::Payable => "payable",
            Self::CreditCard => "credit_card",
            Self::OtherCurrentLiability => "other_current_liability",
            Self::LongTermLiability => "long_term_liability",
            Self::Fund => "fund",
            Self::Income => "income",
            Self::OtherIncome => "other_income",
            Self::DirectCost => "direct_cost",
            Self::Expense => "expense",
            Self::OtherExpense => "other_expense",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.as_str() == s)
    }

    /// Everything's type key (`accounts.js` `ACCOUNT_TYPES`).
    pub fn everything_type(self) -> &'static str {
        match self {
            Self::Bank => "bank",
            Self::Receivable => "ar",
            Self::OtherCurrentAsset => "otherCurrentAsset",
            Self::FixedAsset => "fixedAsset",
            Self::OtherAsset => "otherAsset",
            Self::Payable => "ap",
            Self::CreditCard => "creditCard",
            Self::OtherCurrentLiability => "otherCurrentLiability",
            Self::LongTermLiability => "longTermLiability",
            Self::Fund => "equity",
            Self::Income => "income",
            Self::OtherIncome => "otherIncome",
            Self::DirectCost => "cogs",
            Self::Expense => "expense",
            Self::OtherExpense => "otherExpense",
        }
    }

    pub fn from_everything(t: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.everything_type() == t)
    }

    /// Normal balance: +1 debit, −1 credit.
    pub fn sign(self) -> Decimal {
        match self {
            Self::Bank
            | Self::Receivable
            | Self::OtherCurrentAsset
            | Self::FixedAsset
            | Self::OtherAsset
            | Self::DirectCost
            | Self::Expense
            | Self::OtherExpense => Decimal::ONE,
            _ => Decimal::NEGATIVE_ONE,
        }
    }

    fn order(self) -> usize {
        Self::ALL.iter().position(|g| *g == self).unwrap_or(0)
    }

    pub fn is_balance_sheet(self) -> bool {
        self.order() <= Self::Fund.order()
    }

    /// The ledger account type a group belongs to (`asset` … `expense`).
    pub fn account_type(self) -> &'static str {
        match self {
            Self::Bank
            | Self::Receivable
            | Self::OtherCurrentAsset
            | Self::FixedAsset
            | Self::OtherAsset => "asset",
            Self::Payable
            | Self::CreditCard
            | Self::OtherCurrentLiability
            | Self::LongTermLiability => "liability",
            Self::Fund => "equity",
            Self::Income | Self::OtherIncome => "income",
            Self::DirectCost | Self::Expense | Self::OtherExpense => "expense",
        }
    }

    /// The group of an account that has none set: by its seeded role, a treasury bank / cash
    /// account, else by its type.
    pub fn default_for(account_type: &str, system_key: Option<&str>, is_bank: bool) -> Self {
        match system_key {
            Some("cash_on_hand" | "bank_main") => return Self::Bank,
            Some("student_receivables") => return Self::Receivable,
            Some("other_income") => return Self::OtherIncome,
            _ => {}
        }
        if is_bank && account_type == "asset" {
            return Self::Bank;
        }
        match account_type {
            "asset" => Self::OtherCurrentAsset,
            "liability" => Self::OtherCurrentLiability,
            "equity" => Self::Fund,
            "income" => Self::Income,
            _ => Self::Expense,
        }
    }
}

// --------------------------------------------------------------------------- dates

fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap_or(NaiveDate::MIN)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (ymd(ny, nm, 1) - Duration::days(1)).day()
}

/// Adds months, clamping the day (31 Jan + 1 month = 28/29 Feb). Port of `addMonths`.
pub fn add_months(d: NaiveDate, months: i32, anchor_day: Option<u32>) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + months;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    ymd(y, m, anchor_day.unwrap_or(d.day()).min(days_in_month(y, m)))
}

pub fn add_years(d: NaiveDate, years: i32) -> NaiveDate {
    add_months(d, years * 12, None)
}

pub fn start_of_month(d: NaiveDate) -> NaiveDate {
    ymd(d.year(), d.month(), 1)
}

pub fn end_of_month(d: NaiveDate) -> NaiveDate {
    ymd(d.year(), d.month(), days_in_month(d.year(), d.month()))
}

pub fn start_of_quarter(d: NaiveDate) -> NaiveDate {
    ymd(d.year(), (d.month0() / 3) * 3 + 1, 1)
}

pub fn end_of_quarter(d: NaiveDate) -> NaiveDate {
    end_of_month(add_months(start_of_quarter(d), 2, None))
}

/// First day of the fiscal year containing `d` (India: `start_month` 4). Port of `fiscalYearStart`.
pub fn fiscal_year_start(d: NaiveDate, start_month: u32) -> NaiveDate {
    let year = if d.month() >= start_month {
        d.year()
    } else {
        d.year() - 1
    };
    ymd(year, start_month, 1)
}

/// The same day a year earlier; a month end maps to that month's end. Port of `yearBefore`.
pub fn year_before(d: NaiveDate) -> NaiveDate {
    if d == end_of_month(d) {
        end_of_month(add_years(d, -1))
    } else {
        add_years(d, -1)
    }
}

/// The period just before `[from, to]`: whole months step back by whole months, any other range
/// by its own length in days. Port of `previousPeriod`.
pub fn previous_period(from: NaiveDate, to: NaiveDate) -> (NaiveDate, NaiveDate) {
    if from == start_of_month(from) && to == end_of_month(to) {
        let months = (to.year() - from.year()) * 12 + to.month() as i32 - from.month() as i32 + 1;
        return (
            add_months(from, -months, None),
            end_of_month(add_months(start_of_month(to), -months, None)),
        );
    }
    let days = (to - from).num_days() + 1;
    (from - Duration::days(days), to - Duration::days(days))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A statement column (a period, or an as-of date).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub key: String,
    pub label: String,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

fn col(key: String, label: String, from: Option<NaiveDate>, to: Option<NaiveDate>) -> Column {
    Column {
        key,
        label,
        from,
        to,
    }
}

/// Month buckets between two dates. Port of `monthBuckets`.
pub fn month_buckets(from: NaiveDate, to: NaiveDate) -> Vec<Column> {
    let mut out = Vec::new();
    let mut cur = start_of_month(from);
    while cur <= to && out.len() < 240 {
        let end = end_of_month(cur);
        out.push(col(
            format!("{:04}-{:02}", cur.year(), cur.month()),
            format!("{} {}", MONTHS[cur.month0() as usize], cur.year()),
            Some(cur.max(from)),
            Some(end.min(to)),
        ));
        cur = add_months(cur, 1, Some(1));
    }
    out
}

/// Quarter buckets. Port of `quarterBuckets` (calendar quarters).
pub fn quarter_buckets(from: NaiveDate, to: NaiveDate) -> Vec<Column> {
    let mut out = Vec::new();
    let mut cur = start_of_quarter(from);
    while cur <= to && out.len() < 80 {
        let end = end_of_quarter(cur);
        let q = cur.month0() / 3 + 1;
        out.push(col(
            format!("{}-Q{q}", cur.year()),
            format!("Q{q} {}", cur.year()),
            Some(cur.max(from)),
            Some(end.min(to)),
        ));
        cur = add_months(cur, 3, Some(1));
    }
    out
}

/// Fiscal-year buckets (`FY2027` = the year ending in 2027). Port of `yearBuckets`.
pub fn year_buckets(from: NaiveDate, to: NaiveDate, start_month: u32) -> Vec<Column> {
    let mut out = Vec::new();
    let mut cur = fiscal_year_start(from, start_month);
    while cur <= to && out.len() < 50 {
        let end = add_years(cur, 1) - Duration::days(1);
        let label = if start_month == 1 {
            cur.year().to_string()
        } else {
            format!("FY{}", end.year())
        };
        out.push(col(
            cur.year().to_string(),
            label,
            Some(cur.max(from)),
            Some(end.min(to)),
        ));
        cur = add_years(cur, 1);
    }
    out
}

fn date_long(d: NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

fn range_label(from: NaiveDate, to: NaiveDate) -> String {
    format!("{} to {}", date_long(from), date_long(to))
}

// --------------------------------------------------------------------------- books

/// A ledger account as the statements see it.
#[derive(Clone, Debug)]
pub struct Account {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub group: StatementGroup,
    pub parent: Option<Uuid>,
}

/// A journal entry referenced by postings (for the general ledger).
#[derive(Clone, Debug, Default)]
pub struct EntryRef {
    pub id: Uuid,
    pub number: String,
    pub description: String,
    pub source_type: String,
}

/// One posting: debit-positive amount on an account.
#[derive(Clone, Debug)]
pub struct Posting {
    pub date: NaiveDate,
    pub account: Uuid,
    pub amount: Decimal,
    pub entry: usize,
    pub memo: String,
}

/// The books: the chart and every posting, in date then entry order.
pub struct Books {
    pub accounts: Vec<Account>,
    by_id: HashMap<Uuid, usize>,
    pub postings: Vec<Posting>,
    pub entries: Vec<EntryRef>,
    pub fiscal_start_month: u32,
    /// The accumulated fund (Everything's retained earnings): prior years' surplus lands here.
    pub accumulated_fund: Option<Uuid>,
    chart: Vec<(usize, u32)>,
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    // Numeric-aware: digit runs compare as numbers (localeCompare { numeric: true }).
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(ch) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(ch);
                        it.next();
                    }
                    s
                };
                let (n, m) = (take(&mut x), take(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                match n.len().cmp(&m.len()).then_with(|| n.cmp(m)) {
                    Ordering::Equal => {}
                    o => return o,
                }
            }
            (Some(c), Some(d)) => match c.to_lowercase().cmp(d.to_lowercase()).then(c.cmp(&d)) {
                Ordering::Equal => {
                    x.next();
                    y.next();
                }
                o => return o,
            },
        }
    }
}

impl Books {
    pub fn new(
        accounts: Vec<Account>,
        mut postings: Vec<Posting>,
        entries: Vec<EntryRef>,
        fiscal_start_month: u32,
        accumulated_fund: Option<Uuid>,
    ) -> Self {
        // Stable: same-day postings keep their entry order.
        postings.sort_by_key(|p| p.date);
        let by_id: HashMap<Uuid, usize> = accounts
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id, i))
            .collect();
        // Chart order: group, then code, then name, each sub-account under its parent
        // (`sortChart`).
        let mut children: HashMap<Option<Uuid>, Vec<usize>> = HashMap::new();
        for (i, a) in accounts.iter().enumerate() {
            let parent = a.parent.filter(|p| by_id.contains_key(p));
            children.entry(parent).or_default().push(i);
        }
        for list in children.values_mut() {
            list.sort_by(|&i, &j| {
                let (a, b) = (&accounts[i], &accounts[j]);
                a.group
                    .order()
                    .cmp(&b.group.order())
                    .then_with(|| natural_cmp(&a.code, &b.code))
                    .then_with(|| natural_cmp(&a.name, &b.name))
            });
        }
        let mut chart = Vec::new();
        fn walk(
            p: Option<Uuid>,
            depth: u32,
            kids: &HashMap<Option<Uuid>, Vec<usize>>,
            acc: &[Account],
            out: &mut Vec<(usize, u32)>,
        ) {
            for &i in kids.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
                out.push((i, depth));
                if depth < 8 {
                    walk(Some(acc[i].id), depth + 1, kids, acc, out);
                }
            }
        }
        walk(None, 0, &children, &accounts, &mut chart);
        Books {
            accounts,
            by_id,
            postings,
            entries,
            fiscal_start_month,
            accumulated_fund,
            chart,
        }
    }

    fn account(&self, id: Uuid) -> Option<&Account> {
        self.by_id.get(&id).map(|&i| &self.accounts[i])
    }

    /// "Parent:Child" (`accountLabel`).
    pub fn full_name(&self, id: Uuid) -> String {
        let mut names = Vec::new();
        let mut cur = self.account(id);
        let mut guard = 0;
        while let Some(a) = cur {
            names.push(a.name.clone());
            cur = a.parent.and_then(|p| self.account(p));
            guard += 1;
            if guard > 16 {
                break;
            }
        }
        names.reverse();
        names.join(":")
    }

    /// Raw (debit-positive) activity per account over an inclusive range (`None` = open).
    pub fn activity(
        &self,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> HashMap<Uuid, Decimal> {
        let mut out = HashMap::new();
        for p in &self.postings {
            if from.is_some_and(|f| p.date < f) {
                continue;
            }
            if to.is_some_and(|t| p.date > t) {
                break;
            }
            *out.entry(p.account).or_insert(Decimal::ZERO) += p.amount;
        }
        out
    }

    fn natural(&self, act: &HashMap<Uuid, Decimal>) -> HashMap<Uuid, Decimal> {
        act.iter()
            .filter_map(|(id, v)| self.account(*id).map(|a| (*id, *v * a.group.sign())))
            .collect()
    }

    /// Surplus (net income, natural sign) over a range.
    fn surplus(&self, from: Option<NaiveDate>, to: Option<NaiveDate>) -> Decimal {
        let mut total = Decimal::ZERO;
        for (id, v) in self.activity(from, to) {
            if self
                .account(id)
                .is_some_and(|a| !a.group.is_balance_sheet())
            {
                total -= v;
            }
        }
        total
    }
}

// ---------------------------------------------------------------------------- rows

fn money(d: Decimal) -> String {
    let d = if d.is_zero() { Decimal::ZERO } else { d };
    format!("{:.2}", d.round_dp(2))
}

fn cell<S: serde::Serializer>(v: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(d) => s.serialize_some(&money(*d)),
        None => s.serialize_none(),
    }
}

fn cells<S: serde::Serializer>(v: &[Option<Decimal>], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(v.len()))?;
    for x in v {
        seq.serialize_element(&x.map(money))?;
    }
    seq.end()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Section,
    Account,
    Subtotal,
    Total,
    Grand,
    Row,
}

/// One statement line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementRow {
    pub kind: RowKind,
    /// Stable structure key (Everything's English label) for headings and totals.
    pub key: Option<String>,
    pub label: String,
    pub depth: u32,
    /// One cell per column; `null` = blank. Decimal strings (percent cells: basis points).
    #[schema(value_type = Vec<Option<String>>)]
    #[serde(serialize_with = "cells")]
    pub values: Vec<Option<Decimal>>,
    pub account_id: Option<Uuid>,
    pub emphasis: bool,
    /// General ledger lines: date, entry number, description, source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<LedgerLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LedgerLine {
    pub date: NaiveDate,
    pub entry_id: Uuid,
    pub entry_number: String,
    pub description: String,
    pub source_type: String,
}

fn heading(key: &str, label: &str, depth: u32, n: usize) -> StatementRow {
    StatementRow {
        kind: RowKind::Section,
        key: Some(key.into()),
        label: label.into(),
        depth,
        values: vec![None; n],
        account_id: None,
        emphasis: false,
        detail: None,
    }
}

fn line(kind: RowKind, key: &str, label: &str, depth: u32, values: &[Decimal]) -> StatementRow {
    StatementRow {
        kind,
        key: Some(key.into()),
        label: label.into(),
        depth,
        values: values.iter().copied().map(Some).collect(),
        account_id: None,
        emphasis: false,
        detail: None,
    }
}

fn emph(mut r: StatementRow) -> StatementRow {
    r.emphasis = true;
    r
}

fn add(a: &[Decimal], b: &[Decimal]) -> Vec<Decimal> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn sub(a: &[Decimal], b: &[Decimal]) -> Vec<Decimal> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// Hierarchical rows for the accounts of some groups, with "Total for" subtotals under parents
/// that have sub-accounts (`accountTree`). Returns the rows and the section total.
fn account_tree(
    books: &Books,
    groups: &[StatementGroup],
    values_for: &dyn Fn(Uuid) -> Vec<Decimal>,
    n: usize,
    show_zero: bool,
) -> (Vec<StatementRow>, Vec<Decimal>) {
    let ids: Vec<Uuid> = books
        .chart
        .iter()
        .map(|(i, _)| &books.accounts[*i])
        .filter(|a| groups.contains(&a.group))
        .map(|a| a.id)
        .collect();
    let in_set = |id: &Uuid| ids.contains(id);
    let mut by_parent: HashMap<Option<Uuid>, Vec<Uuid>> = HashMap::new();
    for id in &ids {
        let a = books.account(*id).expect("chart account");
        let p = a.parent.filter(in_set);
        by_parent.entry(p).or_default().push(*id);
    }
    let mut rows = Vec::new();
    fn walk(
        books: &Books,
        by_parent: &HashMap<Option<Uuid>, Vec<Uuid>>,
        values_for: &dyn Fn(Uuid) -> Vec<Decimal>,
        n: usize,
        show_zero: bool,
        parent: Option<Uuid>,
        depth: u32,
        rows: &mut Vec<StatementRow>,
    ) -> Vec<Decimal> {
        let mut acc = vec![Decimal::ZERO; n];
        for &id in by_parent.get(&parent).map(Vec::as_slice).unwrap_or(&[]) {
            let a = books.account(id).expect("chart account");
            let own = values_for(id);
            if by_parent.get(&Some(id)).is_some_and(|c| !c.is_empty()) {
                let saved = rows.len();
                let below = walk(
                    books,
                    by_parent,
                    values_for,
                    n,
                    show_zero,
                    Some(id),
                    depth + 1,
                    rows,
                );
                let combined = add(&own, &below);
                if combined.iter().any(|v| !v.is_zero()) || show_zero {
                    let header = StatementRow {
                        kind: RowKind::Section,
                        key: None,
                        label: a.name.clone(),
                        depth,
                        values: if own.iter().any(|v| !v.is_zero()) {
                            own.iter().copied().map(Some).collect()
                        } else {
                            vec![None; n]
                        },
                        account_id: Some(id),
                        emphasis: false,
                        detail: None,
                    };
                    rows.insert(saved, header);
                    rows.push(StatementRow {
                        kind: RowKind::Subtotal,
                        key: None,
                        label: format!("Total for {}", a.name),
                        depth,
                        values: combined.iter().copied().map(Some).collect(),
                        account_id: Some(id),
                        emphasis: false,
                        detail: None,
                    });
                } else {
                    rows.truncate(saved);
                }
                acc = add(&acc, &combined);
            } else {
                if own.iter().any(|v| !v.is_zero()) || show_zero {
                    rows.push(StatementRow {
                        kind: RowKind::Account,
                        key: None,
                        label: a.name.clone(),
                        depth,
                        values: own.iter().copied().map(Some).collect(),
                        account_id: Some(id),
                        emphasis: false,
                        detail: None,
                    });
                }
                acc = add(&acc, &own);
            }
        }
        acc
    }
    let total = walk(
        books, &by_parent, values_for, n, show_zero, None, 1, &mut rows,
    );
    (rows, total)
}

/// A computed statement.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub columns: Vec<Column>,
    pub rows: Vec<StatementRow>,
    /// Surplus of the period (income & expenditure, first column when comparing) or the net
    /// cash change (cash flow).
    #[schema(value_type = Option<String>)]
    #[serde(serialize_with = "cell")]
    pub net: Option<Decimal>,
    /// Balance sheet: assets = funds + liabilities in every column.
    pub balanced: Option<bool>,
    /// Cash flow: the net cash change equals the movement on cash and bank accounts.
    pub reconciled: Option<bool>,
}

// ------------------------------------------------------------ income & expenditure

/// How the columns of a period statement split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Columns {
    Total,
    Month,
    Quarter,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    PreviousPeriod,
    PreviousYear,
}

/// Basis points of change, rounded half up (`Math.round`); `None` against zero.
fn change_bp(c: Decimal, p: Decimal) -> Option<Decimal> {
    if p.is_zero() {
        return None;
    }
    let x = (c - p) * Decimal::from(10_000) / p.abs();
    Some((x + Decimal::new(5, 1)).floor())
}

fn with_change(rows: &mut [StatementRow]) {
    for r in rows {
        match (
            r.values.first().copied().flatten(),
            r.values.get(1).copied().flatten(),
        ) {
            (Some(c), Some(p)) => r.values = vec![Some(c), Some(p), Some(c - p), change_bp(c, p)],
            _ => r.values.extend([None, None]),
        }
    }
}

/// Drops top-level sections with no rows and a zero total (`trimEmptySections`).
fn trim_empty_sections(rows: Vec<StatementRow>) -> Vec<StatementRow> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let r = &rows[i];
        if r.kind == RowKind::Section && r.depth == 0 && r.account_id.is_none() {
            let close = r.key.as_ref().map(|k| format!("Total for {k}"));
            let mut j = i + 1;
            while j < rows.len()
                && !(rows[j].kind == RowKind::Total && rows[j].depth == 0 && rows[j].key == close)
            {
                j += 1;
            }
            if j == i + 1
                && rows
                    .get(j)
                    .is_some_and(|t| t.values.iter().all(|v| v.is_none_or(|x| x.is_zero())))
            {
                i = j + 1;
                continue;
            }
        }
        out.push(rows[i].clone());
        i += 1;
    }
    out
}

/// Income & expenditure (profit and loss) for `[from, to]`. Port of `profitAndLoss`.
pub fn income_expenditure(
    books: &Books,
    from: NaiveDate,
    to: NaiveDate,
    columns: Columns,
    compare: Option<Compare>,
) -> Statement {
    let mut buckets = match columns {
        Columns::Total => vec![],
        Columns::Month => month_buckets(from, to),
        Columns::Quarter => quarter_buckets(from, to),
        Columns::Year => year_buckets(from, to, books.fiscal_start_month),
    };
    let total = col("total".into(), "Total".into(), Some(from), Some(to));
    if buckets.len() > 1 {
        buckets.push(total);
    } else {
        buckets = vec![total];
    }
    if let Some(c) = compare {
        let (pf, pt) = match c {
            Compare::PreviousYear => (add_years(from, -1), year_before(to)),
            Compare::PreviousPeriod => previous_period(from, to),
        };
        buckets = vec![
            col("cur".into(), range_label(from, to), Some(from), Some(to)),
            col("prev".into(), range_label(pf, pt), Some(pf), Some(pt)),
        ];
    }
    let acts: Vec<HashMap<Uuid, Decimal>> = buckets
        .iter()
        .map(|b| books.natural(&books.activity(b.from, b.to)))
        .collect();
    let n = buckets.len();
    let vals = |id: Uuid| -> Vec<Decimal> {
        acts.iter()
            .map(|m| m.get(&id).copied().unwrap_or_default())
            .collect()
    };
    let mut rows = Vec::new();
    let mut section =
        |key: &str, label: &str, groups: &[StatementGroup], rows: &mut Vec<StatementRow>| {
            let (tree, total) = account_tree(books, groups, &vals, n, false);
            rows.push(heading(key, label, 0, n));
            rows.extend(tree);
            rows.push(line(
                RowKind::Total,
                &format!("Total for {key}"),
                &format!("Total {}", label.to_lowercase()),
                0,
                &total,
            ));
            total
        };
    use StatementGroup as G;
    let income = section("Income", "Income", &[G::Income], &mut rows);
    let cogs = section(
        "Cost of Goods Sold",
        "Direct costs",
        &[G::DirectCost],
        &mut rows,
    );
    let gross = sub(&income, &cogs);
    rows.push(line(
        RowKind::Grand,
        "Gross Profit",
        "Gross surplus",
        0,
        &gross,
    ));
    let expenses = section("Expenses", "Expenditure", &[G::Expense], &mut rows);
    let operating = sub(&gross, &expenses);
    rows.push(line(
        RowKind::Grand,
        "Net Operating Income",
        "Operating surplus",
        0,
        &operating,
    ));
    let other_income = section("Other Income", "Other income", &[G::OtherIncome], &mut rows);
    let other_exp = section(
        "Other Expenses",
        "Other expenditure",
        &[G::OtherExpense],
        &mut rows,
    );
    let other = sub(&other_income, &other_exp);
    rows.push(line(
        RowKind::Grand,
        "Net Other Income",
        "Net other income",
        0,
        &other,
    ));
    let net = add(&operating, &other);
    rows.push(emph(line(
        RowKind::Grand,
        "Net Income",
        "Surplus / (deficit) for the period",
        0,
        &net,
    )));
    let mut cols = buckets.clone();
    if compare.is_some() {
        with_change(&mut rows);
        cols.push(col("change".into(), "₹ change".into(), None, None));
        cols.push(col("pct".into(), "% change".into(), None, None));
    }
    let net_value = if compare.is_some() {
        net[0]
    } else {
        net[n - 1]
    };
    Statement {
        id: "income_expenditure".into(),
        title: "Income & expenditure".into(),
        subtitle: range_label(from, to),
        columns: cols,
        rows: trim_empty_sections(rows),
        net: Some(net_value),
        balanced: None,
        reconciled: None,
    }
}

// --------------------------------------------------------------------- balance sheet

/// Balance-sheet columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SheetColumns {
    Total,
    Month,
    Quarter,
}

/// Balance sheet as of a date. Prior years' surplus is folded into the accumulated fund and the
/// current fiscal year's shows as its own line. Port of `balanceSheet`.
pub fn balance_sheet(
    books: &Books,
    as_of: NaiveDate,
    columns: SheetColumns,
    compare_previous_year: bool,
) -> Statement {
    let mut dates: Vec<(String, NaiveDate)> = vec![("total".into(), as_of)];
    if compare_previous_year {
        dates = vec![("cur".into(), as_of), ("prev".into(), year_before(as_of))];
    } else if columns != SheetColumns::Total {
        let step = if columns == SheetColumns::Month { 1 } else { 3 };
        let base = if columns == SheetColumns::Month {
            start_of_month(as_of)
        } else {
            start_of_quarter(as_of)
        };
        dates = (0..=5)
            .rev()
            .map(|k: i32| {
                let d = if k == 0 {
                    as_of
                } else {
                    add_months(base, -(k - 1) * step, None) - Duration::days(1)
                };
                (format!("d{k}"), d)
            })
            .collect();
    }
    let vals: Vec<(HashMap<Uuid, Decimal>, Decimal, Decimal)> = dates
        .iter()
        .map(|(_, d)| {
            let fy = fiscal_year_start(*d, books.fiscal_start_month);
            (
                books.natural(&books.activity(None, Some(*d))),
                books.surplus(None, Some(fy - Duration::days(1))),
                books.surplus(Some(fy), Some(*d)),
            )
        })
        .collect();
    let n = dates.len();
    let fund = books.accumulated_fund;
    let values_for = |id: Uuid| -> Vec<Decimal> {
        vals.iter()
            .map(|(m, prior, _)| {
                m.get(&id).copied().unwrap_or_default()
                    + if Some(id) == fund {
                        *prior
                    } else {
                        Decimal::ZERO
                    }
            })
            .collect()
    };
    use StatementGroup as G;
    let mut rows = Vec::new();
    let mut group =
        |key: &str, label: &str, groups: &[G], depth: u32, rows: &mut Vec<StatementRow>| {
            let (tree, total) = account_tree(books, groups, &values_for, n, false);
            if tree.is_empty() && total.iter().all(Decimal::is_zero) {
                return total;
            }
            rows.push(heading(key, label, depth, n));
            rows.extend(tree.into_iter().map(|mut r| {
                r.depth += depth;
                r
            }));
            rows.push(line(
                RowKind::Subtotal,
                &format!("Total {key}"),
                &format!("Total {}", label.to_lowercase()),
                depth,
                &total,
            ));
            total
        };
    rows.push(heading("Assets", "Assets", 0, n));
    rows.push(heading("Current Assets", "Current assets", 1, n));
    let bank = group("Bank Accounts", "Cash and bank", &[G::Bank], 2, &mut rows);
    let ar = group(
        "Accounts Receivable",
        "Receivables",
        &[G::Receivable],
        2,
        &mut rows,
    );
    let oca = group(
        "Other Current Assets",
        "Other current assets",
        &[G::OtherCurrentAsset],
        2,
        &mut rows,
    );
    let current = add(&add(&bank, &ar), &oca);
    rows.push(line(
        RowKind::Subtotal,
        "Total Current Assets",
        "Total current assets",
        1,
        &current,
    ));
    let fixed = group(
        "Fixed Assets",
        "Fixed assets",
        &[G::FixedAsset],
        1,
        &mut rows,
    );
    let other = group(
        "Other Assets",
        "Other assets",
        &[G::OtherAsset],
        1,
        &mut rows,
    );
    let assets = add(&add(&current, &fixed), &other);
    rows.push(emph(line(
        RowKind::Total,
        "Total Assets",
        "Total assets",
        0,
        &assets,
    )));
    rows.push(heading(
        "Liabilities and Equity",
        "Funds and liabilities",
        0,
        n,
    ));
    rows.push(heading("Liabilities", "Liabilities", 1, n));
    rows.push(heading("Current Liabilities", "Current liabilities", 2, n));
    let ap = group("Accounts Payable", "Payables", &[G::Payable], 3, &mut rows);
    let cc = group(
        "Credit Cards",
        "Credit cards",
        &[G::CreditCard],
        3,
        &mut rows,
    );
    let ocl = group(
        "Other Current Liabilities",
        "Other current liabilities",
        &[G::OtherCurrentLiability],
        3,
        &mut rows,
    );
    let cur_liab = add(&add(&ap, &cc), &ocl);
    rows.push(line(
        RowKind::Subtotal,
        "Total Current Liabilities",
        "Total current liabilities",
        2,
        &cur_liab,
    ));
    let ltl = group(
        "Long-Term Liabilities",
        "Long-term liabilities",
        &[G::LongTermLiability],
        2,
        &mut rows,
    );
    let liabilities = add(&cur_liab, &ltl);
    rows.push(line(
        RowKind::Subtotal,
        "Total Liabilities",
        "Total liabilities",
        1,
        &liabilities,
    ));
    rows.push(heading("Equity", "Funds", 1, n));
    let (eq_rows, eq_total) = account_tree(books, &[G::Fund], &values_for, n, false);
    rows.extend(eq_rows.into_iter().map(|mut r| {
        r.depth += 1;
        r
    }));
    let ni: Vec<Decimal> = vals.iter().map(|(_, _, cur)| *cur).collect();
    rows.push(line(
        RowKind::Account,
        "Net Income",
        "Surplus for the year to date",
        2,
        &ni,
    ));
    let equity = add(&eq_total, &ni);
    rows.push(line(
        RowKind::Subtotal,
        "Total Equity",
        "Total funds",
        1,
        &equity,
    ));
    let le = add(&liabilities, &equity);
    rows.push(emph(line(
        RowKind::Total,
        "Total Liabilities and Equity",
        "Total funds and liabilities",
        0,
        &le,
    )));
    Statement {
        id: "balance_sheet".into(),
        title: "Balance sheet".into(),
        subtitle: format!("As of {}", date_long(as_of)),
        columns: dates
            .iter()
            .map(|(k, d)| col(k.clone(), date_long(*d), None, Some(*d)))
            .collect(),
        rows,
        net: None,
        balanced: Some(assets == le),
        reconciled: None,
    }
}

// ------------------------------------------------------------------------- cash flow

/// Statement of cash flows, indirect method, for `[from, to]`. Port of `cashFlow`.
pub fn cash_flow(books: &Books, from: NaiveDate, to: NaiveDate) -> Statement {
    let act = books.activity(Some(from), Some(to));
    let ni = books.surplus(Some(from), Some(to));
    let mut rows = vec![
        heading("Operating Activities", "Operating activities", 0, 1),
        line(
            RowKind::Account,
            "Net Income",
            "Surplus for the period",
            1,
            &[ni],
        ),
    ];
    let (mut op, mut inv, mut fin) = (Vec::new(), Vec::new(), Vec::new());
    let mut cash_change = Decimal::ZERO;
    use StatementGroup as G;
    for (i, _) in &books.chart {
        let a = &books.accounts[*i];
        if !a.group.is_balance_sheet() {
            continue;
        }
        let Some(v) = act.get(&a.id).copied() else {
            continue;
        };
        if a.group == G::Bank {
            cash_change += v;
            continue;
        }
        if v.is_zero() {
            continue;
        }
        let r = StatementRow {
            kind: RowKind::Account,
            key: None,
            label: books.full_name(a.id),
            depth: 2,
            values: vec![Some(-v)],
            account_id: Some(a.id),
            emphasis: false,
            detail: None,
        };
        match a.group {
            G::FixedAsset | G::OtherAsset => inv.push(r),
            G::LongTermLiability | G::Fund => fin.push(r),
            _ => op.push(r),
        }
    }
    let total = |l: &[StatementRow]| l.iter().filter_map(|r| r.values[0]).sum::<Decimal>();
    let (t_op, t_inv, t_fin) = (total(&op), total(&inv), total(&fin));
    if !op.is_empty() {
        rows.push(heading(
            "Adjustments to reconcile Net Income to Net Cash provided by operations",
            "Adjustments for working capital",
            1,
            1,
        ));
        rows.extend(op);
        rows.push(line(
            RowKind::Subtotal,
            "Total Adjustments",
            "Total adjustments",
            1,
            &[t_op],
        ));
    }
    let operating = ni + t_op;
    rows.push(line(
        RowKind::Total,
        "Net cash provided by operating activities",
        "Net cash from operating activities",
        0,
        &[operating],
    ));
    rows.push(heading(
        "Investing Activities",
        "Investing activities",
        0,
        1,
    ));
    rows.extend(inv.into_iter().map(|mut r| {
        r.depth = 1;
        r
    }));
    rows.push(line(
        RowKind::Total,
        "Net cash provided by investing activities",
        "Net cash from investing activities",
        0,
        &[t_inv],
    ));
    rows.push(heading(
        "Financing Activities",
        "Financing activities",
        0,
        1,
    ));
    rows.extend(fin.into_iter().map(|mut r| {
        r.depth = 1;
        r
    }));
    rows.push(line(
        RowKind::Total,
        "Net cash provided by financing activities",
        "Net cash from financing activities",
        0,
        &[t_fin],
    ));
    let net = operating + t_inv + t_fin;
    rows.push(line(
        RowKind::Grand,
        "Net cash increase for period",
        "Net increase in cash and bank",
        0,
        &[net],
    ));
    let opening = books.activity(None, Some(from - Duration::days(1)));
    let begin: Decimal = books
        .accounts
        .iter()
        .filter(|a| a.group == G::Bank)
        .filter_map(|a| opening.get(&a.id))
        .sum();
    rows.push(line(
        RowKind::Account,
        "Cash at beginning of period",
        "Cash and bank at the beginning",
        0,
        &[begin],
    ));
    rows.push(emph(line(
        RowKind::Grand,
        "Cash at end of period",
        "Cash and bank at the end",
        0,
        &[begin + net],
    )));
    Statement {
        id: "cash_flow".into(),
        title: "Cash flow statement".into(),
        subtitle: range_label(from, to),
        columns: vec![col("total".into(), "Total".into(), Some(from), Some(to))],
        rows,
        net: Some(net),
        balanced: None,
        reconciled: Some(net == cash_change),
    }
}

// -------------------------------------------------------------------- general ledger

/// The general ledger for `[from, to]`: per account an opening balance (income and expenditure
/// accounts restart at each fiscal year), one line per journal entry with a running balance, and
/// the period total. Values: `[amount, balance]` on the account's normal side. Port of
/// `generalLedger`.
pub fn general_ledger(
    books: &Books,
    from: NaiveDate,
    to: NaiveDate,
    accounts: Option<&[Uuid]>,
) -> Statement {
    let fy = fiscal_year_start(from, books.fiscal_start_month);
    let mut rows = Vec::new();
    for (i, _) in &books.chart {
        let a = &books.accounts[*i];
        if accounts.is_some_and(|l| !l.contains(&a.id)) {
            continue;
        }
        let sign = a.group.sign();
        let mut opening = Decimal::ZERO;
        let mut lines: Vec<(usize, NaiveDate, Decimal)> = Vec::new();
        for p in books.postings.iter().filter(|p| p.account == a.id) {
            if p.date > to {
                break;
            }
            if p.date < from {
                if a.group.is_balance_sheet() || p.date >= fy {
                    opening += p.amount;
                }
                continue;
            }
            match lines.last_mut() {
                Some(last) if last.0 == p.entry => last.2 += p.amount,
                _ => lines.push((p.entry, p.date, p.amount)),
            }
        }
        if lines.is_empty() && opening.is_zero() {
            continue;
        }
        let name = books.full_name(a.id);
        rows.push(StatementRow {
            kind: RowKind::Section,
            key: None,
            label: format!("{} {}", a.code, name).trim().to_owned(),
            depth: 0,
            values: vec![None, None],
            account_id: Some(a.id),
            emphasis: false,
            detail: None,
        });
        rows.push(StatementRow {
            kind: RowKind::Row,
            key: Some("Beginning Balance".into()),
            label: "Opening balance".into(),
            depth: 1,
            values: vec![None, Some(opening * sign)],
            account_id: Some(a.id),
            emphasis: false,
            detail: None,
        });
        let mut running = opening;
        for (e, date, amount) in lines {
            running += amount;
            let entry = books.entries.get(e).cloned().unwrap_or_default();
            rows.push(StatementRow {
                kind: RowKind::Row,
                key: None,
                label: entry.description.clone(),
                depth: 1,
                values: vec![Some(amount * sign), Some(running * sign)],
                account_id: Some(a.id),
                emphasis: false,
                detail: Some(LedgerLine {
                    date,
                    entry_id: entry.id,
                    entry_number: entry.number,
                    description: entry.description,
                    source_type: entry.source_type,
                }),
            });
        }
        rows.push(StatementRow {
            kind: RowKind::Subtotal,
            key: None,
            label: format!("Total for {name}"),
            depth: 0,
            values: vec![Some((running - opening) * sign), Some(running * sign)],
            account_id: Some(a.id),
            emphasis: false,
            detail: None,
        });
    }
    Statement {
        id: "general_ledger".into(),
        title: "General ledger".into(),
        subtitle: range_label(from, to),
        columns: vec![
            col("amount".into(), "Amount".into(), Some(from), Some(to)),
            col("balance".into(), "Balance".into(), Some(from), Some(to)),
        ],
        rows,
        net: None,
        balanced: None,
        reconciled: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn periods() {
        assert_eq!(fiscal_year_start(d("2027-03-31"), 4), d("2026-04-01"));
        assert_eq!(fiscal_year_start(d("2026-04-01"), 4), d("2026-04-01"));
        assert_eq!(year_before(d("2028-02-29")), d("2027-02-28"));
        assert_eq!(year_before(d("2025-02-28")), d("2024-02-29"));
        assert_eq!(
            previous_period(d("2026-09-01"), d("2026-09-30")),
            (d("2026-08-01"), d("2026-08-31"))
        );
        assert_eq!(
            previous_period(d("2026-04-01"), d("2027-03-31")),
            (d("2025-04-01"), d("2026-03-31"))
        );
        assert_eq!(
            previous_period(d("2026-09-10"), d("2026-09-19")),
            (d("2026-08-31"), d("2026-09-09"))
        );
        let y = year_buckets(d("2026-01-15"), d("2027-06-30"), 4);
        assert_eq!(
            y.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            ["FY2026", "FY2027", "FY2028"]
        );
        assert_eq!(month_buckets(d("2026-01-31"), d("2026-03-02")).len(), 3);
        assert_eq!(
            change_bp(Decimal::from(150), Decimal::from(-100)),
            Some(Decimal::from(25_000))
        );
        assert_eq!(
            change_bp(Decimal::from(1), Decimal::from(8)),
            Some(Decimal::from(-8750))
        );
    }

    #[test]
    fn statements_balance() {
        let id = |n: u128| Uuid::from_u128(n);
        let acc = |n, name: &str, g| Account {
            id: id(n),
            code: format!("{n}"),
            name: name.into(),
            group: g,
            parent: None,
        };
        use StatementGroup as G;
        let accounts = vec![
            acc(1, "Bank", G::Bank),
            acc(2, "Fees receivable", G::Receivable),
            acc(3, "Accumulated fund", G::Fund),
            acc(4, "Tuition fees", G::Income),
            acc(5, "Salaries", G::Expense),
            acc(6, "Furniture", G::FixedAsset),
        ];
        let p = |date: &str, a, amt: i64, e| Posting {
            date: d(date),
            account: id(a),
            amount: Decimal::from(amt),
            entry: e,
            memo: String::new(),
        };
        let postings = vec![
            p("2026-03-10", 2, 1000, 0),
            p("2026-03-10", 4, -1000, 0),
            p("2026-04-05", 1, 800, 1),
            p("2026-04-05", 2, -800, 1),
            p("2026-05-01", 5, 300, 2),
            p("2026-05-01", 1, -300, 2),
            p("2026-06-01", 6, 200, 3),
            p("2026-06-01", 1, -200, 3),
            p("2026-06-02", 2, 500, 4),
            p("2026-06-02", 4, -500, 4),
        ];
        let books = Books::new(
            accounts,
            postings,
            vec![EntryRef::default(); 5],
            4,
            Some(id(3)),
        );
        let bs = balance_sheet(&books, d("2026-06-30"), SheetColumns::Total, false);
        assert_eq!(bs.balanced, Some(true));
        let fund = bs
            .rows
            .iter()
            .find(|r| r.account_id == Some(id(3)))
            .unwrap();
        assert_eq!(fund.values, vec![Some(Decimal::from(1000))]);
        let ie = income_expenditure(
            &books,
            d("2026-04-01"),
            d("2026-06-30"),
            Columns::Total,
            None,
        );
        assert_eq!(ie.net, Some(Decimal::from(200)));
        assert!(!ie
            .rows
            .iter()
            .any(|r| r.key.as_deref() == Some("Other Expenses")));
        let cf = cash_flow(&books, d("2026-04-01"), d("2026-06-30"));
        assert_eq!(
            (cf.net, cf.reconciled),
            (Some(Decimal::from(300)), Some(true))
        );
        let gl = general_ledger(&books, d("2026-04-01"), d("2026-06-30"), Some(&[id(4)]));
        assert_eq!(
            gl.rows[1].values,
            vec![None, Some(Decimal::ZERO)],
            "income restarts each fiscal year"
        );
        assert_eq!(
            gl.rows.last().unwrap().values,
            vec![Some(Decimal::from(500)), Some(Decimal::from(500))]
        );
    }
}
