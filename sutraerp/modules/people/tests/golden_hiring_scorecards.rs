//! Golden vectors for `hr::scorecards`, generated from Everything's original
//! `hiring/src/core/{scorecards,guides,screening,reminders}.js` by `tools/golden/generate.mjs`.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use sutra_people::hr::scorecards::{
    average_rating, default_guide_for, fields_or_default, missing_scorecards, normalize_answers,
    normalize_guide_answers, normalize_guides, normalize_rating_fields,
    normalize_screening_questions, screening_mismatches, summarize, validate_scorecard, Interview,
    Scorecard,
};

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    input: Input,
    expected: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Card {
    author: Value,
    recommendation: Value,
    ratings: Map<String, Value>,
    candidate_id: String,
    interview_id: Option<String>,
    created_on: String,
}
#[derive(Deserialize)]
struct Cand {
    id: String,
    status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    today: String,
    fields: Value,
    scorecards: Vec<Card>,
    guides: Value,
    answers_g: Value,
    questions: Value,
    answers: Value,
    interviews: Vec<Interview>,
    candidates: Vec<Cand>,
    kind: String,
    stage: String,
}

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), |f| json!((f * 1e9).round() / 1e9)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        other => other,
    }
}

fn eq(got: Value, want: &Value, what: &str) {
    assert_eq!(norm(got), norm(want.clone()), "{what}");
}

fn to<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).expect("json")
}

fn author(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_default()
}

#[test]
fn hiring_scorecards_match_everything() {
    let fx: Fixture =
        serde_json::from_str(include_str!("golden/hiring_scorecards.json")).expect("fixture");
    assert!(fx.cases.len() >= 150);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let w = |k: &str| format!("case {ci} {k}");
        eq(to(normalize_rating_fields(&i.fields)), &x["normalizedFields"], &w("normalizedFields"));
        let rubric = fields_or_default(&i.fields);
        eq(to(&rubric), &x["rubric"], &w("rubric"));
        let cards: Vec<Scorecard> = i
            .scorecards
            .iter()
            .map(|s| Scorecard {
                candidate_id: s.candidate_id.clone(),
                interview_id: s.interview_id.clone(),
                recommendation: s.recommendation.as_str().map(str::to_owned),
                ratings: s.ratings.clone(),
                created_on: Some(s.created_on.clone()),
            })
            .collect();
        let validation: Vec<Vec<String>> = i
            .scorecards
            .iter()
            .map(|s| validate_scorecard(&author(&s.author), &s.recommendation, &s.ratings))
            .collect();
        eq(to(validation), &x["validation"], &w("validation"));
        let avgs: Vec<Option<f64>> = cards.iter().map(average_rating).collect();
        eq(to(avgs), &x["averages"], &w("averages"));
        eq(to(summarize(&cards, &rubric)), &x["summary"], &w("summary"));
        let guides = normalize_guides(&i.guides);
        eq(to(&guides), &x["guides"], &w("guides"));
        let dg = default_guide_for(&guides, &i.kind, &i.stage).map(|g| g.id.clone());
        eq(to(dg), &x["defaultGuide"], &w("defaultGuide"));
        eq(to(normalize_guide_answers(&i.answers_g)), &x["guideAnswers"], &w("guideAnswers"));
        let qs = normalize_screening_questions(&i.questions);
        eq(to(&qs), &x["questions"], &w("questions"));
        let ans = normalize_answers(&i.answers);
        eq(to(&ans), &x["answers"], &w("answers"));
        let mism: Vec<String> = screening_mismatches(
            ans.as_deref().unwrap_or_default(),
            qs.as_deref().unwrap_or_default(),
        )
        .iter()
        .map(|q| q.id.clone())
        .collect();
        eq(to(mism), &x["mismatches"], &w("mismatches"));
        let live: HashSet<String> = i
            .candidates
            .iter()
            .filter(|c| c.status != "rejected" && c.status != "withdrawn")
            .map(|c| c.id.clone())
            .collect();
        let missing: Vec<String> = missing_scorecards(&i.interviews, &cards, &live, &i.today)
            .iter()
            .map(|i| i.id.clone())
            .collect();
        eq(to(missing), &x["missing"], &w("missing"));
    }
}
