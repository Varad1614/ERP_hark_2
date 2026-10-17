//! **ERP_EXTENSIONS 3B** — hiring scorecards, ported from HeyPuter/everything
//! `src/hiring/src/core/scorecards.js` (rubric, validation, averages, panel summary and
//! leaning), `guides.js` (interview guides and the guide picked for an interview),
//! `screening.js` (screening questions, answers, must-have mismatches) and `reminders.js`
//! `missingScorecards` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! Pure: stored JSON in, cleaned values out. Scorecards snapshot the rubric they were rated
//! against, and guide answers snapshot the question text, so later edits never rewrite
//! feedback already given.

use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};

// ------------------------------------------------------------------ helpers

/// `isSafeId`: `[A-Za-z0-9_-]{1,64}`.
pub fn is_safe_id(v: &str) -> bool {
    (1..=64).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// JS `String(value ?? '')`.
fn js_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.as_f64().map_or_else(|| n.to_string(), js_num),
        Some(Value::Array(a)) => a.iter().map(|x| if x.is_null() { String::new() } else { js_str(Some(x)) }).collect::<Vec<_>>().join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

fn js_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn take(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn trim_take(v: Option<&Value>, max: usize) -> String {
    take(js_str(v).trim(), max)
}

// ------------------------------------------------------------------ rubric

/// A rating dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = HiringRatingField)]
pub struct RatingField {
    pub key: String,
    pub label: String,
}

/// The default rubric.
pub fn default_fields() -> Vec<RatingField> {
    [("skills", "Role skills"), ("experience", "Experience"), ("communication", "Communication"), ("collaboration", "Collaboration")]
        .into_iter()
        .map(|(k, l)| RatingField { key: k.into(), label: l.into() })
        .collect()
}

/// `normalizeRatingFields`: safe unique keys with labels; `None` = use the default.
pub fn normalize_rating_fields(v: &Value) -> Option<Vec<RatingField>> {
    let arr = v.as_array()?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in arr {
        let key = js_str(e.get("key"));
        let label = js_str(e.get("label")).trim().to_owned();
        if !is_safe_id(&key) || label.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        out.push(RatingField { key, label });
    }
    (!out.is_empty()).then_some(out)
}

/// `ratingFieldsForJob` / `ratingFieldsForScorecard`.
pub fn fields_or_default(v: &Value) -> Vec<RatingField> {
    normalize_rating_fields(v).unwrap_or_else(default_fields)
}

/// The four recommendations and their scores.
pub const RECOMMENDATIONS: [(&str, &str, i64); 4] =
    [("strong_yes", "Strong yes", 2), ("yes", "Yes", 1), ("no", "No", -1), ("strong_no", "Strong no", -2)];

/// `recommendationInfo`: the score of a recommendation.
pub fn recommendation_score(v: &str) -> Option<i64> {
    RECOMMENDATIONS.iter().find(|r| r.0 == v).map(|r| r.2)
}

/// `isValidRating`: an integer 1–5.
pub fn is_valid_rating(v: &Value) -> bool {
    v.as_f64().is_some_and(|f| f.fract() == 0.0 && (1.0..=5.0).contains(&f))
}

/// `validateScorecard`.
pub fn validate_scorecard(author: &str, recommendation: &Value, ratings: &Map<String, Value>) -> Vec<String> {
    let mut errors = Vec::new();
    if author.trim().is_empty() {
        errors.push("Add the reviewer\u{2019}s name.".to_owned());
    }
    if recommendation.as_str().and_then(recommendation_score).is_none() {
        errors.push("Pick an overall recommendation.".to_owned());
    }
    for (k, v) in ratings {
        if !v.is_null() && !is_valid_rating(v) {
            errors.push(format!("Rating for {k} must be between 1 and 5."));
        }
    }
    errors
}

/// A scorecard as the summaries read it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scorecard {
    pub candidate_id: String,
    pub interview_id: Option<String>,
    pub recommendation: Option<String>,
    pub ratings: Map<String, Value>,
    /// The local day it was written.
    pub created_on: Option<String>,
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// `averageRating`: mean of the ratings filled in.
pub fn average_rating(s: &Scorecard) -> Option<f64> {
    let v: Vec<f64> = s.ratings.values().filter(|x| is_valid_rating(x)).filter_map(Value::as_f64).collect();
    mean(&v)
}

/// A panel's feedback rolled up.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringScorecardSummary)]
pub struct Summary {
    pub count: usize,
    pub average: Option<f64>,
    /// Average per rubric key (null when nobody rated it).
    pub by_field: BTreeMap<String, Option<f64>>,
    /// Count per recommendation.
    pub recommendations: BTreeMap<String, i64>,
    /// `hire | no_hire | split`, null without scorecards.
    pub leaning: Option<String>,
}

/// `summarizeScorecards`.
pub fn summarize(cards: &[Scorecard], fields: &[RatingField]) -> Summary {
    let mut s = Summary {
        count: cards.len(),
        average: None,
        by_field: BTreeMap::new(),
        recommendations: RECOMMENDATIONS.iter().map(|r| (r.0.to_owned(), 0)).collect(),
        leaning: None,
    };
    if cards.is_empty() {
        return s;
    }
    for f in fields {
        let v: Vec<f64> = cards
            .iter()
            .filter_map(|c| c.ratings.get(&f.key))
            .filter(|x| is_valid_rating(x))
            .filter_map(Value::as_f64)
            .collect();
        s.by_field.insert(f.key.clone(), mean(&v));
    }
    let avgs: Vec<f64> = cards.iter().filter_map(average_rating).collect();
    s.average = mean(&avgs);
    let mut total = 0;
    for c in cards {
        let Some(rec) = c.recommendation.as_deref() else { continue };
        let Some(score) = recommendation_score(rec) else { continue };
        *s.recommendations.entry(rec.to_owned()).or_default() += 1;
        total += score;
    }
    s.leaning = Some(if total > 0 { "hire" } else if total < 0 { "no_hire" } else { "split" }.to_owned());
    s
}

// ------------------------------------------------------------------ guides

/// Interview formats.
pub const INTERVIEW_TYPES: [(&str, &str); 3] = [("phone", "Phone screen"), ("video", "Video call"), ("onsite", "Onsite")];

/// A guide question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = HiringGuideQuestion)]
pub struct GuideQuestion {
    pub id: String,
    pub text: String,
    /// The rubric key it feeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// An interview guide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = HiringGuide)]
pub struct Guide {
    pub id: String,
    pub name: String,
    pub questions: Vec<GuideQuestion>,
    /// Interview formats it opens for.
    pub types: Vec<String>,
    /// Pipeline stages it opens for.
    pub stages: Vec<String>,
}

fn uniq(v: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.filter(|x| seen.insert(x.clone())).collect()
}

/// `normalizeGuides`: up to 12 named guides of up to 20 questions each.
pub fn normalize_guides(v: &Value) -> Vec<Guide> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in arr {
        let id = js_str(e.get("id"));
        let name = trim_take(e.get("name"), 80);
        if !is_safe_id(&id) || name.is_empty() || seen.contains(&id) {
            continue;
        }
        let mut questions = Vec::new();
        let mut seen_q = HashSet::new();
        for q in e.get("questions").and_then(Value::as_array).into_iter().flatten() {
            let qid = js_str(q.get("id"));
            let text = trim_take(q.get("text"), 300);
            if !is_safe_id(&qid) || text.is_empty() || seen_q.contains(&qid) {
                continue;
            }
            seen_q.insert(qid.clone());
            let field = js_str(q.get("field"));
            questions.push(GuideQuestion { id: qid, text, field: is_safe_id(&field).then_some(field) });
            if questions.len() >= 20 {
                break;
            }
        }
        if questions.is_empty() {
            continue;
        }
        seen.insert(id.clone());
        let types = uniq(
            e.get("types")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|t| INTERVIEW_TYPES.iter().any(|i| i.0 == *t))
                .map(str::to_owned),
        );
        let stages = uniq(
            e.get("stages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|s| if s.is_null() { "null".to_owned() } else { js_str(Some(s)) })
                .filter(|s| is_safe_id(s)),
        );
        out.push(Guide { id, name, questions, types, stages });
        if out.len() >= 12 {
            break;
        }
    }
    out
}

/// `defaultGuideFor`: best match on format (2) and stage (1); first wins a tie; none on 0.
pub fn default_guide_for<'a>(guides: &'a [Guide], kind: &str, stage: &str) -> Option<&'a Guide> {
    let mut best = None;
    let mut best_score = 0;
    for g in guides {
        let score = if !kind.is_empty() && g.types.iter().any(|t| t == kind) { 2 } else { 0 }
            + i32::from(!stage.is_empty() && g.stages.iter().any(|s| s == stage));
        if score > best_score {
            best = Some(g);
            best_score = score;
        }
    }
    best
}

/// An answer to a guide question, snapshotted with its text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = HiringGuideAnswer)]
pub struct GuideAnswer {
    pub question: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// `normalizeGuideAnswers`.
pub fn normalize_guide_answers(v: &Value) -> Option<Vec<GuideAnswer>> {
    let arr = v.as_array()?;
    let mut out = Vec::new();
    for e in arr {
        let question = trim_take(e.get("question"), 300);
        let answer = trim_take(e.get("answer"), 2000);
        if question.is_empty() || answer.is_empty() {
            continue;
        }
        let field = js_str(e.get("field"));
        out.push(GuideAnswer { question, answer, field: is_safe_id(&field).then_some(field) });
        if out.len() >= 20 {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

// ------------------------------------------------------------------ screening

/// A screening question on the application form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringScreeningQuestion)]
pub struct ScreeningQuestion {
    pub id: String,
    pub label: String,
    /// `short | long | yesno | choice`.
    pub kind: String,
    pub required: bool,
    /// A yes/no question that should be answered yes (a flag, never a filter).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_have: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// `normalizeScreeningQuestions`: up to 8; `None` = asks nothing extra.
pub fn normalize_screening_questions(v: &Value) -> Option<Vec<ScreeningQuestion>> {
    let arr = v.as_array()?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in arr {
        let id = js_str(e.get("id"));
        let label = take(js_str(e.get("label")).trim(), 160);
        if !is_safe_id(&id) || label.is_empty() || seen.contains(&id) {
            continue;
        }
        let kind = e
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| ["short", "long", "yesno", "choice"].contains(k))
            .unwrap_or("short")
            .to_owned();
        let options: Vec<String> = if kind == "choice" {
            let raw = e.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut o = uniq(
                raw.iter()
                    .map(|x| if x.is_null() { "null".to_owned() } else { js_str(Some(x)) }.trim().to_owned())
                    .filter(|x| !x.is_empty()),
            );
            o.truncate(6);
            o
        } else {
            Vec::new()
        };
        if kind == "choice" && options.is_empty() {
            continue;
        }
        seen.insert(id.clone());
        out.push(ScreeningQuestion {
            id,
            label,
            must_have: kind == "yesno" && truthy(e.get("mustHave")),
            required: truthy(e.get("required")),
            kind,
            options,
        });
        if out.len() >= 8 {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A candidate's answer, stored with the question's label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = HiringScreeningAnswer)]
pub struct Answer {
    pub label: String,
    pub answer: String,
}

/// `normalizeAnswers`.
pub fn normalize_answers(v: &Value) -> Option<Vec<Answer>> {
    let arr = v.as_array()?;
    let mut out = Vec::new();
    for e in arr {
        let label = take(js_str(e.get("label")).trim(), 160);
        let answer = take(js_str(e.get("answer")).trim(), 1000);
        if label.is_empty() || answer.is_empty() {
            continue;
        }
        out.push(Answer { label, answer });
        if out.len() >= 8 {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `isYesAnswer`.
pub fn is_yes_answer(a: &str) -> bool {
    matches!(a.trim().to_ascii_lowercase().as_str(), "yes" | "y" | "true")
}

/// `answerFor`: matched by label, case-insensitively.
pub fn answer_for<'a>(answers: &'a [Answer], q: &ScreeningQuestion) -> Option<&'a str> {
    let key = q.label.trim().to_lowercase();
    answers.iter().find(|a| a.label.trim().to_lowercase() == key).map(|a| a.answer.as_str())
}

/// `screeningMismatches`: must-have questions answered with something other than yes
/// (unanswered is not a mismatch).
pub fn screening_mismatches<'a>(answers: &[Answer], questions: &'a [ScreeningQuestion]) -> Vec<&'a ScreeningQuestion> {
    questions.iter().filter(|q| q.must_have).filter(|q| answer_for(answers, q).is_some_and(|a| !is_yes_answer(a))).collect()
}

// ------------------------------------------------------------------ missing feedback

/// An interview as the reminder reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Interview {
    pub id: String,
    pub candidate_id: String,
    pub date: String,
    /// `scheduled | proposed | cancelled`.
    pub status: String,
}

fn add_days(iso: &str, n: i64) -> String {
    NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .map_or_else(|_| iso.to_owned(), |d| (d + Duration::days(n)).format("%Y-%m-%d").to_string())
}

/// `missingScorecards`: interviews held before yesterday and within 14 days, with no linked
/// scorecard and no unlinked one written on or after the day, for candidates still in play
/// (`archived` lists rejected / withdrawn / unknown ones), newest first.
pub fn missing_scorecards<'a>(
    interviews: &'a [Interview],
    cards: &[Scorecard],
    live: &HashSet<String>,
    today: &str,
) -> Vec<&'a Interview> {
    let yesterday = add_days(today, -1);
    let horizon = add_days(today, -14);
    let covered: HashSet<&str> = cards.iter().filter_map(|s| s.interview_id.as_deref()).filter(|s| !s.is_empty()).collect();
    let mut unlinked: BTreeMap<&str, String> = BTreeMap::new();
    for s in cards.iter().filter(|s| s.interview_id.as_deref().is_none_or(str::is_empty)) {
        let day = s.created_on.clone().unwrap_or_default();
        let cur = unlinked.get(s.candidate_id.as_str()).cloned().unwrap_or_default();
        if day > cur {
            unlinked.insert(s.candidate_id.as_str(), day);
        }
    }
    let mut out: Vec<&Interview> = interviews
        .iter()
        .filter(|i| i.status != "proposed" && i.status != "cancelled")
        .filter(|i| i.date < yesterday && i.date >= horizon && !covered.contains(i.id.as_str()))
        .filter(|i| i.date > unlinked.get(i.candidate_id.as_str()).cloned().unwrap_or_default())
        .filter(|i| live.contains(&i.candidate_id))
        .collect();
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rubric_summary_guides_screening() {
        assert_eq!(fields_or_default(&json!([{"key": "bad key", "label": "x"}])).len(), 4);
        let cards = vec![
            Scorecard { recommendation: Some("strong_yes".into()), ratings: json!({"skills": 5, "experience": 3}).as_object().cloned().unwrap_or_default(), ..Scorecard::default() },
            Scorecard { recommendation: Some("no".into()), ratings: json!({"skills": 4, "communication": 7}).as_object().cloned().unwrap_or_default(), ..Scorecard::default() },
        ];
        let s = summarize(&cards, &default_fields());
        assert_eq!(s.leaning.as_deref(), Some("hire"));
        assert_eq!(s.by_field["skills"], Some(4.5));
        assert_eq!(s.average, Some(4.0));
        let errs = validate_scorecard(" ", &json!("maybe"), &cards[1].ratings);
        assert_eq!(errs.len(), 3);
        let g = normalize_guides(&json!([
            {"id": "a", "name": "Screen", "questions": [{"id": "q", "text": "Why?"}], "types": ["phone"]},
            {"id": "b", "name": "Demo", "questions": [{"id": "q", "text": "Teach"}], "types": ["onsite"], "stages": ["interviewed"]}
        ]));
        assert_eq!(default_guide_for(&g, "onsite", "interviewed").map(|g| g.id.as_str()), Some("b"));
        let q = normalize_screening_questions(&json!([{"id": "net", "label": "Cleared NET/SET?", "kind": "yesno", "mustHave": true}])).unwrap_or_default();
        let a = vec![Answer { label: "cleared net/set?".into(), answer: "No".into() }];
        assert_eq!(screening_mismatches(&a, &q).len(), 1);
    }

}
