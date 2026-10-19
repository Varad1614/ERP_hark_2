//! OpenSearch 2.x HTTP client for the search projections — **owned by slice 9C**
//! (`docs/batch9-contracts.md` §8). Plain REST over `reqwest` (rustls): index creation with the
//! projection mappings (+ read / write alias), `_bulk` upserts / deletes with external versioning
//! (`external_gte`, so a replay of the same version is a no-op and an older one is ignored),
//! `_search` / `_count` always filtered by `tenantId`, and a cluster-health readiness probe.
//!
//! Domain modules never see this type; the composition root wraps it as the
//! `sutra_contracts::platform::search::SearchIndex` port (this crate may only depend on
//! `sutra-kernel`, so it speaks plain JSON).
//!
//! Index naming: physical index `<prefix>-<projection>-v1`, alias `<prefix>-<projection>`. The
//! mappings below are the source of truth; `deploy/opensearch/templates/<projection>.json` holds
//! the same mappings as index templates (a unit test keeps the two identical).

use std::time::Duration;
use sutra_kernel::error::KernelError;

/// Mapping major version (`-v1`).
pub const MAPPING_MAJOR: u32 = 1;

/// Connection settings (`Config::hardening.search`).
#[derive(Clone, Debug)]
pub struct OpenSearchSettings {
    /// `http(s)://host:9200`.
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// `<prefix>` of every index / alias (`sutra-<env>`).
    pub index_prefix: String,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// One document to index.
#[derive(Clone, Debug, PartialEq)]
pub struct BulkDoc {
    pub id: String,
    pub version: i64,
    pub source: serde_json::Value,
}

/// One search hit (`_id`, `_score`, `_source`).
#[derive(Clone, Debug, PartialEq)]
pub struct RawHit {
    pub id: String,
    pub score: f64,
    pub source: serde_json::Value,
}

/// OpenSearch REST client.
#[derive(Clone)]
pub struct OpenSearchClient {
    http: reqwest::Client,
    settings: OpenSearchSettings,
}

impl std::fmt::Debug for OpenSearchClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSearchClient")
            .field("url", &self.settings.url)
            .field("index_prefix", &self.settings.index_prefix)
            .finish_non_exhaustive()
    }
}

fn unavailable(e: impl std::fmt::Display) -> KernelError {
    KernelError::Unavailable(format!("opensearch: {e}"))
}

/// Mappings of one projection (`dynamic: strict`: a document with an unknown member fails).
pub fn mappings(projection: &str) -> Option<serde_json::Value> {
    let keyword = serde_json::json!({ "type": "keyword" });
    let text_kw = serde_json::json!({
        "type": "text",
        "fields": { "keyword": { "type": "keyword", "ignore_above": 256 } }
    });
    let props = match projection {
        "students" => serde_json::json!({
            "id": keyword, "tenantId": keyword, "institutionId": keyword,
            "number": text_kw, "displayName": text_kw, "programme": keyword, "status": keyword
        }),
        "staff" => serde_json::json!({
            "id": keyword, "tenantId": keyword, "institutionId": keyword,
            "number": text_kw, "displayName": text_kw, "department": keyword, "status": keyword
        }),
        "library-catalogue" => serde_json::json!({
            "id": keyword, "tenantId": keyword, "institutionId": keyword,
            "title": text_kw, "authors": text_kw, "isbn": keyword, "subjects": text_kw,
            "availableCopies": { "type": "integer" }
        }),
        _ => return None,
    };
    Some(serde_json::json!({ "dynamic": "strict", "properties": props }))
}

/// Index template body for `deploy/opensearch/templates/<projection>.json` (prefix-agnostic).
pub fn index_template(projection: &str) -> Option<serde_json::Value> {
    let m = mappings(projection)?;
    Some(serde_json::json!({
        "index_patterns": [format!("*-{projection}-v{MAPPING_MAJOR}")],
        "template": {
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": m
        },
        "priority": 100
    }))
}

/// Fields searched for a free-text query, per projection.
fn query_fields(projection: &str) -> Vec<&'static str> {
    match projection {
        "students" | "staff" => vec!["displayName^3", "number^2"],
        "library-catalogue" => vec!["title^3", "authors^2", "subjects", "isbn"],
        _ => vec![],
    }
}

/// `_search` body: tenant filter always, free text optional (pure, unit tested).
pub fn search_body(projection: &str, tenant: &str, q: &str, limit: u32) -> serde_json::Value {
    let filter = serde_json::json!([{ "term": { "tenantId": tenant } }]);
    let q = q.trim();
    let query = if q.is_empty() {
        serde_json::json!({ "bool": { "filter": filter, "must": [{ "match_all": {} }] } })
    } else {
        serde_json::json!({ "bool": { "filter": filter, "must": [{
            "multi_match": {
                "query": q,
                "type": "bool_prefix",
                "operator": "and",
                "lenient": true,
                "fields": query_fields(projection)
            }
        }] } })
    };
    serde_json::json!({
        "size": limit.clamp(1, 100),
        "track_total_hits": true,
        "query": query,
        "sort": ["_score", { "id": "asc" }]
    })
}

impl OpenSearchClient {
    /// Builds the HTTP client; does not connect.
    pub fn new(settings: OpenSearchSettings) -> Result<Self, String> {
        url::Url::parse(&settings.url).map_err(|e| format!("invalid SUTRA_OPENSEARCH_URL: {e}"))?;
        let http = crate::tls::with_extra_roots(
            reqwest::Client::builder()
                .timeout(settings.timeout)
                .connect_timeout(settings.timeout.min(Duration::from_secs(5))),
        )?
        .build()
        .map_err(|e| format!("opensearch client: {e}"))?;
        Ok(Self { http, settings })
    }

    pub fn settings(&self) -> &OpenSearchSettings {
        &self.settings
    }

    /// `<prefix>-<projection>-v1`.
    pub fn index_name(&self, projection: &str) -> String {
        format!(
            "{}-{projection}-v{MAPPING_MAJOR}",
            self.settings.index_prefix
        )
    }

    /// `<prefix>-<projection>` (read + write alias).
    pub fn alias(&self, projection: &str) -> String {
        format!("{}-{projection}", self.settings.index_prefix)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.settings.url.trim_end_matches('/'), path);
        let r = self.http.request(method, url);
        match &self.settings.user {
            Some(u) => r.basic_auth(u, self.settings.password.as_deref()),
            None => r,
        }
    }

    async fn send(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, serde_json::Value), KernelError> {
        let res = rb.send().await.map_err(unavailable)?;
        let status = res.status();
        let text = res.text().await.map_err(unavailable)?;
        let body = if text.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        };
        if status.is_server_error() {
            return Err(unavailable(format!("{status}: {body}")));
        }
        Ok((status, body))
    }

    fn fail(what: &str, status: reqwest::StatusCode, body: &serde_json::Value) -> KernelError {
        KernelError::internal_msg(format!("opensearch {what}: {status}: {body}"))
    }

    /// Cluster health is green or yellow.
    pub async fn health(&self) -> Result<(), KernelError> {
        let (status, body) = self
            .send(self.req(reqwest::Method::GET, "_cluster/health"))
            .await?;
        if !status.is_success() {
            return Err(unavailable(format!("cluster health {status}")));
        }
        match body.get("status").and_then(|s| s.as_str()) {
            Some("green") | Some("yellow") => Ok(()),
            other => Err(unavailable(format!("cluster status {other:?}"))),
        }
    }

    /// Creates `<prefix>-<projection>-v1` with its mappings and alias when it does not exist.
    pub async fn ensure_index(&self, projection: &str) -> Result<(), KernelError> {
        let index = self.index_name(projection);
        let (status, _) = self.send(self.req(reqwest::Method::HEAD, &index)).await?;
        if status.is_success() {
            return Ok(());
        }
        let mappings = mappings(projection)
            .ok_or_else(|| KernelError::BadRequest(format!("unknown projection {projection}")))?;
        let mut aliases = serde_json::Map::new();
        aliases.insert(self.alias(projection), serde_json::json!({}));
        let body = serde_json::json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": mappings,
            "aliases": aliases,
        });
        let (status, resp) = self
            .send(self.req(reqwest::Method::PUT, &index).json(&body))
            .await?;
        if status.is_success() {
            return Ok(());
        }
        let already = resp
            .pointer("/error/type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t == "resource_already_exists_exception");
        if already {
            Ok(())
        } else {
            Err(Self::fail("create index", status, &resp))
        }
    }

    /// Deletes the physical index of `projection` (missing = fine).
    pub async fn delete_index(&self, projection: &str) -> Result<(), KernelError> {
        let index = self.index_name(projection);
        let (status, resp) = self.send(self.req(reqwest::Method::DELETE, &index)).await?;
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::fail("delete index", status, &resp))
        }
    }

    async fn bulk(&self, ndjson: String) -> Result<u64, KernelError> {
        if ndjson.is_empty() {
            return Ok(0);
        }
        let (status, resp) = self
            .send(
                self.req(reqwest::Method::POST, "_bulk")
                    .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
                    .body(ndjson),
            )
            .await?;
        if !status.is_success() {
            return Err(Self::fail("bulk", status, &resp));
        }
        let mut written = 0;
        let mut errors = vec![];
        for item in resp
            .get("items")
            .and_then(|i| i.as_array())
            .into_iter()
            .flatten()
        {
            let Some(op) = item.as_object().and_then(|o| o.values().next()) else {
                continue;
            };
            let code = op.get("status").and_then(|s| s.as_u64()).unwrap_or(0);
            match code {
                200 | 201 => written += 1,
                // 409: an equal-or-newer version is already indexed; 404: delete of a missing doc.
                404 | 409 => {}
                _ => errors.push(op.get("error").cloned().unwrap_or_default()),
            }
        }
        if errors.is_empty() {
            Ok(written)
        } else {
            Err(KernelError::internal_msg(format!(
                "opensearch bulk: {} item(s) failed, first: {}",
                errors.len(),
                errors[0]
            )))
        }
    }

    /// Indexes `docs` through the alias with `version_type=external_gte`.
    pub async fn bulk_index(&self, projection: &str, docs: &[BulkDoc]) -> Result<u64, KernelError> {
        let alias = self.alias(projection);
        let mut nd = String::new();
        for d in docs {
            let action = serde_json::json!({ "index": {
                "_index": alias, "_id": d.id, "version": d.version, "version_type": "external_gte"
            }});
            nd.push_str(&action.to_string());
            nd.push('\n');
            nd.push_str(&d.source.to_string());
            nd.push('\n');
        }
        self.bulk(nd).await
    }

    /// Deletes `ids` through the alias.
    pub async fn bulk_delete(&self, projection: &str, ids: &[String]) -> Result<u64, KernelError> {
        let alias = self.alias(projection);
        let mut nd = String::new();
        for id in ids {
            let action = serde_json::json!({ "delete": { "_index": alias, "_id": id } });
            nd.push_str(&action.to_string());
            nd.push('\n');
        }
        self.bulk(nd).await
    }

    /// `_refresh` of the alias.
    pub async fn refresh(&self, projection: &str) -> Result<(), KernelError> {
        let path = format!("{}/_refresh", self.alias(projection));
        let (status, resp) = self.send(self.req(reqwest::Method::POST, &path)).await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::fail("refresh", status, &resp))
        }
    }

    /// Tenant-filtered search: `(total, hits)`.
    pub async fn search(
        &self,
        projection: &str,
        tenant: &str,
        q: &str,
        limit: u32,
    ) -> Result<(u64, Vec<RawHit>), KernelError> {
        let path = format!("{}/_search", self.alias(projection));
        let body = search_body(projection, tenant, q, limit);
        let (status, resp) = self
            .send(self.req(reqwest::Method::POST, &path).json(&body))
            .await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok((0, vec![]));
        }
        if !status.is_success() {
            return Err(Self::fail("search", status, &resp));
        }
        let total = resp
            .pointer("/hits/total/value")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let hits = resp
            .pointer("/hits/hits")
            .and_then(|h| h.as_array())
            .into_iter()
            .flatten()
            .filter_map(|h| {
                // Defence in depth: never return a document of another tenant.
                let source = h.get("_source")?.clone();
                if source.get("tenantId").and_then(|t| t.as_str()) != Some(tenant) {
                    return None;
                }
                Some(RawHit {
                    id: h.get("_id")?.as_str()?.to_owned(),
                    score: h.get("_score").and_then(|s| s.as_f64()).unwrap_or(0.0),
                    source,
                })
            })
            .collect();
        Ok((total, hits))
    }

    /// `_count` of the alias (one tenant or all).
    pub async fn count(&self, projection: &str, tenant: Option<&str>) -> Result<u64, KernelError> {
        let path = format!("{}/_count", self.alias(projection));
        let body = match tenant {
            Some(t) => serde_json::json!({ "query": { "term": { "tenantId": t } } }),
            None => serde_json::json!({ "query": { "match_all": {} } }),
        };
        let (status, resp) = self
            .send(self.req(reqwest::Method::POST, &path).json(&body))
            .await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(0);
        }
        if !status.is_success() {
            return Err(Self::fail("count", status, &resp));
        }
        Ok(resp.get("count").and_then(|c| c.as_u64()).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn client() -> OpenSearchClient {
        OpenSearchClient::new(OpenSearchSettings {
            url: "http://localhost:9200".into(),
            user: None,
            password: None,
            index_prefix: "sutra-test".into(),
            timeout: Duration::from_secs(1),
        })
        .unwrap()
    }

    #[test]
    fn names_follow_the_contract() {
        let c = client();
        assert_eq!(c.index_name("students"), "sutra-test-students-v1");
        assert_eq!(c.alias("library-catalogue"), "sutra-test-library-catalogue");
        assert!(OpenSearchClient::new(OpenSearchSettings {
            url: "not a url".into(),
            ..c.settings().clone()
        })
        .is_err());
    }

    #[test]
    fn every_query_filters_on_the_tenant() {
        for q in ["", "aarav"] {
            let b = search_body("students", "t-1", q, 500);
            assert_eq!(b["size"], 100, "limit is clamped");
            assert_eq!(b["query"]["bool"]["filter"][0]["term"]["tenantId"], "t-1");
        }
        let b = search_body("library-catalogue", "t-2", "rust", 10);
        assert_eq!(
            b["query"]["bool"]["must"][0]["multi_match"]["fields"][0],
            "title^3"
        );
    }

    #[test]
    fn mappings_exist_for_every_projection_and_match_the_committed_templates() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/opensearch/templates");
        for p in ["students", "staff", "library-catalogue"] {
            let m = mappings(p).unwrap();
            assert_eq!(m["dynamic"], "strict");
            assert_eq!(m["properties"]["tenantId"]["type"], "keyword");
            let committed: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join(format!("{p}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(
                committed,
                index_template(p).unwrap(),
                "deploy/opensearch/templates/{p}.json drifted from sutra_infra::opensearch::mappings"
            );
        }
        assert!(mappings("nope").is_none());
    }
}
