//! Tamper-evident audit chaining (ADR-L5-0012) — the **pure hashing contract** of Batch 9
//! (`docs/batch9-contracts.md` §4).
//!
//! Every row of `platform.audit_log` is sealed, per tenant and in seal order, into
//! `platform.audit_chain_entry` with
//!
//! ```text
//! entry_hash = SHA-256( "sutra.audit.v1" "\n" hex(prev_hash) "\n" canonical_json(row) )
//! ```
//!
//! where `prev_hash` is the previous entry's hash of the same tenant ([`GENESIS_HASH`] for the
//! first) and `canonical_json` is the row as a JSON object with **sorted keys at every level**, no
//! whitespace, `occurredAt` as RFC 3339 UTC with exactly six fractional digits, absent values as
//! `null` ([`canonical_json`]). Anchors sign the chain head with HMAC-SHA-256 ([`anchor_mac`]).
//!
//! The sealer, the verification API and the anchor job live in 9A (`sutra_persistence::audit_chain`,
//! `sutra_platform::audit`); this module only fixes the bytes so every component (and an external
//! auditor's script) computes identical hashes.

use crate::error::KernelError;
use crate::secret::{hex, hmac_sha256, sha256};
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// Domain-separation prefix and version of the entry hash.
pub const CHAIN_VERSION: &str = "sutra.audit.v1";
/// Domain-separation prefix and version of the anchor MAC.
pub const ANCHOR_VERSION: &str = "sutra.audit.anchor.v1";
/// `prev_hash` of the first entry of every tenant chain.
pub const GENESIS_HASH: [u8; 32] = [0u8; 32];

/// The columns of one `platform.audit_log` row that are covered by the chain (all of them).
#[derive(Clone, Debug, PartialEq)]
pub struct ChainRow<'a> {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub tenant_id: Uuid,
    pub institution_id: Option<Uuid>,
    pub actor_kind: &'a str,
    pub actor_id: Option<&'a str>,
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_id: Option<&'a str>,
    pub outcome: &'a str,
    pub correlation_id: Option<&'a str>,
    pub details: &'a Value,
}

fn opt(v: Option<&str>) -> Value {
    v.map_or(Value::Null, |s| Value::String(s.to_owned()))
}

/// Canonical JSON of a row (sorted keys, compact, fixed timestamp format).
pub fn canonical_json(row: &ChainRow<'_>) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), Value::String(row.id.to_string()));
    obj.insert(
        "occurredAt".into(),
        Value::String(row.occurred_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
    );
    obj.insert("tenantId".into(), Value::String(row.tenant_id.to_string()));
    obj.insert(
        "institutionId".into(),
        row.institution_id
            .map_or(Value::Null, |i| Value::String(i.to_string())),
    );
    obj.insert("actorKind".into(), Value::String(row.actor_kind.to_owned()));
    obj.insert("actorId".into(), opt(row.actor_id));
    obj.insert("action".into(), Value::String(row.action.to_owned()));
    obj.insert("targetType".into(), opt(row.target_type));
    obj.insert("targetId".into(), opt(row.target_id));
    obj.insert("outcome".into(), Value::String(row.outcome.to_owned()));
    obj.insert("correlationId".into(), opt(row.correlation_id));
    obj.insert("details".into(), row.details.clone());
    let mut out = String::new();
    write_canonical(&Value::Object(obj), &mut out);
    out
}

/// Compact JSON with object keys sorted (byte order) at every level, independent of whether
/// `serde_json` preserves insertion order in this build.
pub fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                if let Some(child) = map.get(k.as_str()) {
                    write_canonical(child, out);
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Hash of one chain entry given the previous entry's hash.
pub fn entry_hash(prev_hash: &[u8; 32], row: &ChainRow<'_>) -> [u8; 32] {
    let msg = format!(
        "{CHAIN_VERSION}\n{}\n{}",
        hex(prev_hash),
        canonical_json(row)
    );
    sha256(msg.as_bytes())
}

/// HMAC-SHA-256 anchor over a tenant's chain head (`platform.audit_anchor.mac`). The key comes
/// from OpenBao `kv/sutraerp/<env>/audit` → `anchor_key` (`SUTRA_AUDIT_ANCHOR_KEY`).
pub fn anchor_mac(
    key: &[u8],
    tenant_id: Uuid,
    seq: i64,
    head_hash: &[u8; 32],
) -> Result<[u8; 32], KernelError> {
    let msg = format!("{ANCHOR_VERSION}\n{tenant_id}\n{seq}\n{}", hex(head_hash));
    hmac_sha256(key, msg.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample(details: &Value) -> ChainRow<'_> {
        ChainRow {
            id: Uuid::parse_str("0192f5a0-7c3e-7000-8000-000000000901").unwrap(),
            occurred_at: Utc
                .with_ymd_and_hms(2026, 11, 20, 4, 30, 0)
                .unwrap()
                .checked_add_signed(chrono::Duration::microseconds(123_456))
                .unwrap(),
            tenant_id: Uuid::parse_str("0192f5a0-7c3e-7000-8000-000000000001").unwrap(),
            institution_id: None,
            actor_kind: "user",
            actor_id: Some("admin"),
            action: "institution.institution.rename",
            target_type: Some("institution"),
            target_id: None,
            outcome: "succeeded",
            correlation_id: None,
            details,
        }
    }

    #[test]
    fn canonical_json_sorts_keys_and_fixes_the_timestamp() {
        let details = serde_json::json!({"z": 1, "a": {"y": [true, null], "b": "x"}});
        let json = canonical_json(&sample(&details));
        assert_eq!(
            json,
            "{\"action\":\"institution.institution.rename\",\"actorId\":\"admin\",\"actorKind\":\"user\",\
             \"correlationId\":null,\"details\":{\"a\":{\"b\":\"x\",\"y\":[true,null]},\"z\":1},\
             \"id\":\"0192f5a0-7c3e-7000-8000-000000000901\",\"institutionId\":null,\
             \"occurredAt\":\"2026-11-20T04:30:00.123456Z\",\"outcome\":\"succeeded\",\"targetId\":null,\
             \"targetType\":\"institution\",\"tenantId\":\"0192f5a0-7c3e-7000-8000-000000000001\"}"
        );
    }

    #[test]
    fn entry_hash_matches_the_published_vector_and_chains() {
        let details = serde_json::json!({"z": 1, "a": {"y": [true, null], "b": "x"}});
        let row = sample(&details);
        let first = entry_hash(&GENESIS_HASH, &row);
        assert_eq!(hex(&first), ENTRY_VECTOR);
        let second = entry_hash(&first, &row);
        assert_ne!(first, second, "the previous hash is part of the input");
        let tampered = serde_json::json!({"z": 2, "a": {"y": [true, null], "b": "x"}});
        assert_ne!(entry_hash(&GENESIS_HASH, &sample(&tampered)), first);
    }

    #[test]
    fn anchor_mac_is_keyed() {
        let head = [7u8; 32];
        let t = Uuid::parse_str("0192f5a0-7c3e-7000-8000-000000000001").unwrap();
        let a = anchor_mac(b"key-a", t, 42, &head).unwrap();
        let b = anchor_mac(b"key-b", t, 42, &head).unwrap();
        assert_ne!(a, b);
        assert_eq!(a, anchor_mac(b"key-a", t, 42, &head).unwrap());
        assert_ne!(a, anchor_mac(b"key-a", t, 43, &head).unwrap());
    }

    /// SHA-256 of the sample row on the genesis hash (docs/batch9-contracts.md §4.2 test vector).
    const ENTRY_VECTOR: &str = "890495a8bd29552c520acb3b573f2823ed646c54e8029aaa6dfa52c7f59a5067";
}
