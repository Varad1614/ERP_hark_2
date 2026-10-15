//! Part 1 Stage 2 (demo) regression tests — the environment configuration a developer starts from.
//!
//! * DEF-P1-DEMO-002: `.env.example` (documented as "copy to .env") did not parse as a dotenv file
//!   (`SUTRA_MAIL_FROM=SutraERP <no-reply@sutra.dev>` unquoted), and `main` ignored the parse
//!   error, so every variable after that line was silently missing. Found by the full-stack demo
//!   workflow (`sutra-api check-config` with `SUTRA_ENV_FILE=.env.example`). These tests parse the
//!   whole file and load it into a `Config` exactly like `serve` would.
//!
//! No database needed (runs in the plain `cargo test` job as well as with `db-tests`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use sutra_api::config::{AuthProvider, Config, Source};

struct Vars(HashMap<String, String>);

impl Source for Vars {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).filter(|v| !v.trim().is_empty()).cloned()
    }
}

fn env_example() -> HashMap<String, String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example");
    let mut vars = HashMap::new();
    for (n, item) in dotenvy::from_path_iter(path)
        .expect("open .env.example")
        .enumerate()
    {
        match item {
            Ok((k, v)) => {
                vars.insert(k, v);
            }
            Err(e) => panic!(".env.example entry {} does not parse: {e}", n + 1),
        }
    }
    vars
}

#[test]
fn env_example_parses_completely() {
    let vars = env_example();
    assert_eq!(
        vars.get("SUTRA_MAIL_FROM").map(String::as_str),
        Some("SutraERP <no-reply@sutra.dev>"),
        "the display-name sender must survive quoting"
    );
    for key in [
        "SUTRA_ENV",
        "DATABASE_URL",
        "SUTRA_AUTH_MODE",
        "SUTRA_SMTP_HOST",
    ] {
        assert!(vars.contains_key(key), "{key} missing from .env.example");
    }
}

#[test]
fn env_example_is_a_valid_development_configuration() {
    let cfg = Config::load(&Vars(env_example())).expect(".env.example is a valid configuration");
    assert_eq!(cfg.auth_provider, AuthProvider::Dev);
    assert!(!cfg.public_url.is_empty());
}
