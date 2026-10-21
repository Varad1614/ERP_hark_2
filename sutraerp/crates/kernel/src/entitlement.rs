//! Module entitlements and licence state (Part 2 · Stage 7, S7-entitle;
//! `docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`).
//!
//! * [`Registry`] — the **one** canonical module / package registry, embedded from
//!   `catalog/entitlements.json` (the web app imports the same file). Fifteen licensable modules
//!   (each mapped to its crate), the always-available core, four packages, the dependency graph
//!   and the server-side classification of `/api/v1/<area>/…` request paths onto modules.
//! * [`TenantEntitlement`] → [`EffectiveEntitlement`] — what one tenant has bought (package,
//!   add-on modules, licence status, validity window, grace days) and what that means *now*
//!   ([`effective_status`]: active → grace → expired; suspended overrides).
//! * [`decide`] — the access rule the HTTP gate (`sutra_web::entitlement`) applies to every
//!   authenticated request: an inactive module answers `403 entitlement.module_inactive`; an
//!   expired licence keeps reads and data export but refuses writes
//!   (`403 entitlement.licence_expired`); a suspended licence leaves only the platform
//!   administration area (platform administrators) and data export
//!   (`403 entitlement.licence_suspended`). Sign-in / sign-out and the entitlement status itself
//!   are always reachable.
//! * [`EntitlementStore`] — port to the stored entitlement (PostgreSQL adapter in
//!   `sutra_platform::entitlement`), [`Clock`] — injectable time (tests drive expiry / renewal).
//!
//! Framework-neutral: no Axum, no SQLx.

use crate::error::{FieldViolation, KernelError};
use crate::scope::TenantId;
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

/// Embedded copy of `catalog/entitlements.json` (shared with the web app).
pub const REGISTRY_JSON: &str = include_str!("../../../catalog/entitlements.json");

/// Stable error code: the request belongs to a module the tenant has not activated.
pub const MODULE_INACTIVE: &str = "entitlement.module_inactive";
/// Stable error code: the licence has expired (after its grace period); writes are refused.
pub const LICENCE_EXPIRED: &str = "entitlement.licence_expired";
/// Stable error code: the licence is suspended; only platform administration and export remain.
pub const LICENCE_SUSPENDED: &str = "entitlement.licence_suspended";

/// Longest grace period a licence may carry (days).
pub const MAX_GRACE_DAYS: i32 = 365;

/// One licensable module.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModuleSpec {
    /// Stable module key (`examinations`).
    pub key: String,
    pub name: String,
    /// Crate that implements it (`sutra-exams`).
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// `false` = defined commercially but not implemented in this build (cannot be activated).
    pub activatable: bool,
    /// Modules that must be active as well.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// An always-available part of the product (never gated).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoreSpec {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// A commercial package: a dependency-closed set of modules.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageSpec {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub modules: Vec<String>,
}

/// Classification rules of one API area (`/api/v1/<area>/…`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct AreaRules {
    /// Module of a path no rule matches (`None` = core, never gated).
    pub default: Option<String>,
    /// `[needle, module]`: a non-id path segment containing `needle` selects `module`; tried in order.
    #[serde(default)]
    pub rules: Vec<(String, String)>,
}

/// UI-only mirrors (navigation filtering); never the control.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct WebMirror {
    #[serde(default)]
    pub routes: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRegistry {
    version: u32,
    modules: Vec<ModuleSpec>,
    #[serde(default)]
    core: Vec<CoreSpec>,
    packages: Vec<PackageSpec>,
    default_package: String,
    api: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    imports: BTreeMap<String, String>,
    #[serde(default)]
    web: WebMirror,
}

/// The parsed, validated registry.
#[derive(Clone, Debug)]
pub struct Registry {
    pub version: u32,
    pub modules: Vec<ModuleSpec>,
    pub core: Vec<CoreSpec>,
    pub packages: Vec<PackageSpec>,
    /// Package of a tenant without a stored entitlement when the default is `package` (see
    /// [`DefaultEntitlement`]).
    pub default_package: String,
    pub api: BTreeMap<String, AreaRules>,
    /// Data-import entity → module whose records it writes (`staff` → `hr`, `fee_heads` →
    /// `fees`, …). Entities not listed (institution set-up) are core. Enforced by the import
    /// handlers (`sutra_platform::dataimport`) through [`EntitlementPolicy::require_module`].
    pub imports: BTreeMap<String, String>,
    pub web: WebMirror,
}

impl Registry {
    /// Parses and validates a registry document. Every inconsistency is reported.
    pub fn parse(json: &str) -> Result<Self, String> {
        let raw: RawRegistry =
            serde_json::from_str(json).map_err(|e| format!("entitlement registry: {e}"))?;
        let mut api = BTreeMap::new();
        for (area, v) in raw.api {
            if area.starts_with('$') {
                continue;
            }
            let rules: AreaRules = serde_json::from_value(v)
                .map_err(|e| format!("entitlement registry: api.{area}: {e}"))?;
            api.insert(area, rules);
        }
        let reg = Self {
            version: raw.version,
            modules: raw.modules,
            core: raw.core,
            packages: raw.packages,
            default_package: raw.default_package,
            api,
            imports: raw
                .imports
                .into_iter()
                .filter(|(k, _)| !k.starts_with('$'))
                .collect(),
            web: raw.web,
        };
        let problems = reg.problems();
        if problems.is_empty() {
            Ok(reg)
        } else {
            Err(format!("entitlement registry: {}", problems.join("; ")))
        }
    }

    fn problems(&self) -> Vec<String> {
        let mut out = vec![];
        let mut keys = BTreeSet::new();
        for m in &self.modules {
            if !keys.insert(m.key.as_str()) {
                out.push(format!("duplicate module `{}`", m.key));
            }
        }
        for m in &self.modules {
            for r in &m.requires {
                if !keys.contains(r.as_str()) {
                    out.push(format!("module `{}` requires unknown `{r}`", m.key));
                }
                if r == &m.key {
                    out.push(format!("module `{}` requires itself", m.key));
                }
            }
        }
        let mut pkgs = BTreeSet::new();
        for p in &self.packages {
            if !pkgs.insert(p.key.as_str()) {
                out.push(format!("duplicate package `{}`", p.key));
            }
            for m in &p.modules {
                match self.module(m) {
                    None => out.push(format!("package `{}` lists unknown module `{m}`", p.key)),
                    Some(spec) if !spec.activatable => out.push(format!(
                        "package `{}` lists module `{m}` that cannot be activated",
                        p.key
                    )),
                    Some(_) => {}
                }
            }
            let set: BTreeSet<String> = p.modules.iter().cloned().collect();
            for (m, r) in self.missing_dependencies(&set) {
                out.push(format!(
                    "package `{}`: `{m}` requires `{r}`, which it does not include",
                    p.key
                ));
            }
        }
        if self.package(&self.default_package).is_none() {
            out.push(format!(
                "defaultPackage `{}` is not a package",
                self.default_package
            ));
        }
        for (area, rules) in &self.api {
            for m in rules
                .default
                .iter()
                .chain(rules.rules.iter().map(|(_, m)| m))
            {
                if !keys.contains(m.as_str()) {
                    out.push(format!("api.{area} names unknown module `{m}`"));
                }
            }
        }
        for (entity, m) in &self.imports {
            if !keys.contains(m.as_str()) {
                out.push(format!("imports.{entity} names unknown module `{m}`"));
            }
        }
        for m in self.web.routes.values().chain(self.web.workspaces.values()) {
            if !keys.contains(m.as_str()) {
                out.push(format!("web names unknown module `{m}`"));
            }
        }
        out
    }

    pub fn module(&self, key: &str) -> Option<&ModuleSpec> {
        self.modules.iter().find(|m| m.key == key)
    }

    pub fn package(&self, key: &str) -> Option<&PackageSpec> {
        self.packages.iter().find(|p| p.key == key)
    }

    /// `(module, missing requirement)` pairs of `set`.
    pub fn missing_dependencies(&self, set: &BTreeSet<String>) -> Vec<(String, String)> {
        let mut out = vec![];
        for m in set {
            if let Some(spec) = self.module(m) {
                for r in &spec.requires {
                    if !set.contains(r) {
                        out.push((m.clone(), r.clone()));
                    }
                }
            }
        }
        out
    }

    /// Validates a requested activation (package + add-on modules) and returns the active set.
    /// Rejects unknown or non-activatable keys and any module whose dependencies are not active
    /// (`examinations` without `academics`, `payroll` without `hr`, …).
    pub fn resolve(
        &self,
        package: &str,
        add_ons: &[String],
    ) -> Result<BTreeSet<String>, Vec<FieldViolation>> {
        let mut errors = vec![];
        let mut set = BTreeSet::new();
        match self.package(package) {
            Some(p) => set.extend(p.modules.iter().cloned()),
            None => errors.push(FieldViolation::new(
                "package",
                "unknown_package",
                format!("`{package}` is not a package"),
            )),
        }
        for (i, m) in add_ons.iter().enumerate() {
            match self.module(m) {
                None => errors.push(FieldViolation::new(
                    format!("addOnModules[{i}]"),
                    "unknown_module",
                    format!("`{m}` is not a module"),
                )),
                Some(spec) if !spec.activatable => errors.push(FieldViolation::new(
                    format!("addOnModules[{i}]"),
                    "module_not_activatable",
                    format!("`{m}` is not available in this build"),
                )),
                Some(_) => {
                    set.insert(m.clone());
                }
            }
        }
        if errors.is_empty() {
            for (m, r) in self.missing_dependencies(&set) {
                errors.push(FieldViolation::new(
                    "addOnModules",
                    "dependency_missing",
                    format!("`{m}` requires `{r}`; add `{r}` or choose a package that includes it"),
                ));
            }
        }
        if errors.is_empty() {
            Ok(set)
        } else {
            Err(errors)
        }
    }

    /// Module a data-import entity writes into; `None` = core (institution set-up).
    pub fn import_module(&self, entity: &str) -> Option<&str> {
        self.imports.get(entity).map(String::as_str)
    }

    /// Module a request path belongs to; `None` = core (never gated).
    pub fn classify(&self, path: &str) -> Option<&str> {
        let rest = path.strip_prefix("/api/v1/")?;
        let mut segments = rest.split('/').filter(|s| !s.is_empty());
        let area = segments.next()?;
        let rules = self.api.get(area)?;
        let segments: Vec<&str> = segments
            .filter(|s| uuid::Uuid::parse_str(s).is_err())
            .collect();
        for (needle, module) in &rules.rules {
            if segments.iter().any(|s| s.contains(needle.as_str())) {
                return Some(module.as_str());
            }
        }
        rules.default.as_deref()
    }
}

/// The embedded registry. `Err` only if `catalog/entitlements.json` is invalid (a unit test pins
/// it valid; the API refuses to start otherwise and the gate fails closed).
pub fn registry() -> Result<&'static Registry, &'static str> {
    static REG: OnceLock<Result<Registry, String>> = OnceLock::new();
    match REG.get_or_init(|| Registry::parse(REGISTRY_JSON)) {
        Ok(r) => Ok(r),
        Err(e) => Err(e.as_str()),
    }
}

/// Licence status: stored ([`TenantEntitlement::status`]) or effective ([`effective_status`]).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LicenceStatus {
    /// Full access.
    Active,
    /// Past `validUntil` but within the grace days: full access + renewal banner.
    Grace,
    /// Past the grace period (or not yet valid): read-only access and data export.
    Expired,
    /// Administratively suspended: platform administration and data export only.
    Suspended,
}

impl LicenceStatus {
    pub const ALL: [LicenceStatus; 4] = [
        LicenceStatus::Active,
        LicenceStatus::Grace,
        LicenceStatus::Expired,
        LicenceStatus::Suspended,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            LicenceStatus::Active => "active",
            LicenceStatus::Grace => "grace",
            LicenceStatus::Expired => "expired",
            LicenceStatus::Suspended => "suspended",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Where an entitlement came from.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementOrigin {
    /// A row in `platform.tenant_entitlement` (set through the approved admin API).
    Stored,
    /// No stored row: the configured default (`SUTRA_ENTITLEMENT_DEFAULT`).
    Default,
}

/// What one tenant holds (stored form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantEntitlement {
    /// `None` = no package (core only + add-ons).
    pub package: Option<String>,
    pub add_on_modules: Vec<String>,
    pub status: LicenceStatus,
    pub valid_from: DateTime<Utc>,
    /// `None` = perpetual.
    pub valid_until: Option<DateTime<Utc>>,
    pub grace_days: i32,
    pub version: i64,
    pub origin: EntitlementOrigin,
}

/// Effective status at `now`: `suspended` and `expired` are kept; otherwise before `valid_from`
/// → expired (not yet valid), up to `valid_until` → active (`grace` when stored as grace), up to
/// `valid_until + grace_days` → grace, after that → expired.
pub fn effective_status(
    stored: LicenceStatus,
    valid_from: DateTime<Utc>,
    valid_until: Option<DateTime<Utc>>,
    grace_days: i32,
    now: DateTime<Utc>,
) -> LicenceStatus {
    match stored {
        LicenceStatus::Suspended => return LicenceStatus::Suspended,
        LicenceStatus::Expired => return LicenceStatus::Expired,
        LicenceStatus::Active | LicenceStatus::Grace => {}
    }
    if now < valid_from {
        return LicenceStatus::Expired;
    }
    let Some(until) = valid_until else {
        return stored;
    };
    if now <= until {
        return stored;
    }
    if now <= until + Duration::days(i64::from(grace_days.clamp(0, MAX_GRACE_DAYS))) {
        LicenceStatus::Grace
    } else {
        LicenceStatus::Expired
    }
}

/// What a tenant may use right now (served by `GET /api/v1/platform/entitlement`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveEntitlement {
    pub package: Option<String>,
    pub add_on_modules: Vec<String>,
    /// Active module keys (sorted).
    pub modules: Vec<String>,
    /// Effective licence status at `asOf`.
    pub status: LicenceStatus,
    /// Status as stored (before the validity window is applied).
    pub stored_status: LicenceStatus,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub grace_days: i32,
    /// End of the grace period (`validUntil + graceDays`), when the licence has an end.
    pub grace_ends_at: Option<DateTime<Utc>>,
    /// Whole days until the licence (status `active`) or its grace period (status `grace`) ends.
    pub days_remaining: Option<i64>,
    /// Writes are refused (`expired` / `suspended`).
    pub read_only: bool,
    pub origin: EntitlementOrigin,
    pub version: i64,
    pub as_of: DateTime<Utc>,
}

impl EffectiveEntitlement {
    pub fn has_module(&self, key: &str) -> bool {
        self.modules.iter().any(|m| m == key)
    }

    /// Audit C3: the subset every session needs for navigation and the read-only banner —
    /// active `modules`, `status`, `readOnly`, `origin` and `asOf`. The commercial terms
    /// (package, add-ons, stored status, validity window, grace, version) are blanked:
    /// `validFrom` becomes the Unix epoch, the optional dates `null`, numbers `0`.
    pub fn summary_only(mut self) -> Self {
        self.package = None;
        self.add_on_modules = Vec::new();
        self.stored_status = self.status;
        self.valid_from = DateTime::<Utc>::UNIX_EPOCH;
        self.valid_until = None;
        self.grace_days = 0;
        self.grace_ends_at = None;
        self.days_remaining = None;
        self.version = 0;
        self
    }
}

impl TenantEntitlement {
    /// Perpetual, active entitlement to `package` (the configured default).
    pub fn default_package(package: &str) -> Self {
        Self {
            package: Some(package.to_owned()),
            add_on_modules: vec![],
            status: LicenceStatus::Active,
            valid_from: DateTime::<Utc>::UNIX_EPOCH,
            valid_until: None,
            grace_days: 0,
            version: 0,
            origin: EntitlementOrigin::Default,
        }
    }

    /// Effective entitlement at `now`. Unknown or non-activatable keys (a registry that changed
    /// after the row was written) are dropped, never granted.
    pub fn effective(&self, reg: &Registry, now: DateTime<Utc>) -> EffectiveEntitlement {
        let mut modules = BTreeSet::new();
        if let Some(p) = self.package.as_deref().and_then(|p| reg.package(p)) {
            modules.extend(p.modules.iter().cloned());
        }
        modules.extend(self.add_on_modules.iter().cloned());
        modules.retain(|m| reg.module(m).is_some_and(|s| s.activatable));
        let status = effective_status(
            self.status,
            self.valid_from,
            self.valid_until,
            self.grace_days,
            now,
        );
        let grace_ends_at = self
            .valid_until
            .map(|u| u + Duration::days(i64::from(self.grace_days.clamp(0, MAX_GRACE_DAYS))));
        let days_remaining = match status {
            LicenceStatus::Active => self.valid_until.map(|u| (u - now).num_days()),
            LicenceStatus::Grace => grace_ends_at.map(|g| (g - now).num_days()),
            LicenceStatus::Expired | LicenceStatus::Suspended => None,
        };
        EffectiveEntitlement {
            package: self.package.clone(),
            add_on_modules: self.add_on_modules.clone(),
            modules: modules.into_iter().collect(),
            status,
            stored_status: self.status,
            valid_from: self.valid_from,
            valid_until: self.valid_until,
            grace_days: self.grace_days,
            grace_ends_at,
            days_remaining,
            read_only: matches!(status, LicenceStatus::Expired | LicenceStatus::Suspended),
            origin: self.origin,
            version: self.version,
            as_of: now,
        }
    }
}

/// Entitlement of a tenant without a stored row (`SUTRA_ENTITLEMENT_DEFAULT`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultEntitlement {
    /// Perpetual, active entitlement to this package (default: the registry's `defaultPackage`,
    /// `complete_erp`, so tenants created before entitlements existed keep every module).
    Package(String),
    /// Nothing: core only until an entitlement is set (strict SaaS mode).
    Nothing,
}

impl DefaultEntitlement {
    /// Strict parse of `SUTRA_ENTITLEMENT_DEFAULT`: a package key or `none`.
    pub fn parse(s: &str, reg: &Registry) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("none") {
            return Some(Self::Nothing);
        }
        reg.package(s).map(|p| Self::Package(p.key.clone()))
    }

    pub fn entitlement(&self) -> TenantEntitlement {
        match self {
            Self::Package(p) => TenantEntitlement::default_package(p),
            Self::Nothing => TenantEntitlement {
                package: None,
                ..TenantEntitlement::default_package("")
            },
        }
    }
}

/// Injectable time source.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Settable clock for tests (expiry, grace, renewal).
#[derive(Debug)]
pub struct ManualClock(RwLock<DateTime<Utc>>);

impl ManualClock {
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(RwLock::new(at))
    }

    pub fn set(&self, at: DateTime<Utc>) {
        match self.0.write() {
            Ok(mut g) => *g = at,
            Err(p) => *p.into_inner() = at,
        }
    }

    pub fn advance(&self, by: Duration) {
        let now = self.now();
        self.set(now + by);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        match self.0.read() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }
}

/// Port: the stored entitlement of a tenant (`None` = no row).
#[async_trait::async_trait]
pub trait EntitlementStore: Send + Sync {
    async fn load(&self, tenant: TenantId) -> Result<Option<TenantEntitlement>, KernelError>;
}

/// Store + clock + default: resolves the effective entitlement of a tenant.
#[derive(Clone)]
pub struct EntitlementPolicy {
    pub store: Arc<dyn EntitlementStore>,
    pub clock: Arc<dyn Clock>,
    pub default: DefaultEntitlement,
}

impl EntitlementPolicy {
    pub fn new(
        store: Arc<dyn EntitlementStore>,
        clock: Arc<dyn Clock>,
        default: DefaultEntitlement,
    ) -> Self {
        Self {
            store,
            clock,
            default,
        }
    }

    /// Same policy with another clock (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Effective entitlement of `tenant` now. Fails closed (`Unavailable`) when the registry is
    /// invalid or the store cannot be read.
    pub async fn effective(&self, tenant: TenantId) -> Result<EffectiveEntitlement, KernelError> {
        let reg = registry().map_err(|e| KernelError::Unavailable(e.to_owned()))?;
        let stored = self.store.load(tenant).await?;
        let ent = stored.unwrap_or_else(|| self.default.entitlement());
        Ok(ent.effective(reg, self.clock.now()))
    }

    /// Whether `module` is active for `tenant` now (fails closed like [`Self::effective`]).
    /// Cross-module ports use it to treat an inactive module's facts as not applicable (e.g.
    /// Finance dues when `fees` is not active).
    pub async fn module_active(&self, tenant: TenantId, module: &str) -> Result<bool, KernelError> {
        Ok(self.effective(tenant).await?.has_module(module))
    }

    /// `Ok` when `module` is active for `tenant`, else
    /// [`KernelError::Entitlement`] with [`MODULE_INACTIVE`] (`403`).
    pub async fn require_module(&self, tenant: TenantId, module: &str) -> Result<(), KernelError> {
        if self.module_active(tenant, module).await? {
            return Ok(());
        }
        Err(module_inactive(module))
    }
}

/// The [`KernelError::Entitlement`] of an inactive module (same detail as the HTTP gate).
pub fn module_inactive(module: &str) -> KernelError {
    let name = registry()
        .ok()
        .and_then(|r| r.module(module))
        .map_or(module, |s| s.name.as_str());
    KernelError::Entitlement {
        code: MODULE_INACTIVE,
        detail: format!(
            "the {name} module (`{module}`) is not active for this institution; \
             a platform administrator can activate it"
        ),
    }
}

/// Paths reachable whatever the entitlement: sign-in / sign-out / session and the entitlement
/// status + administration (so a suspended tenant can still see why and be renewed).
pub fn is_always_allowed(path: &str) -> bool {
    !path.starts_with("/api/")
        || path == "/api/v1/auth"
        || path.starts_with("/api/v1/auth/")
        || path == "/api/v1/platform/entitlement"
        || path.starts_with("/api/v1/platform/entitlement/")
        || path.starts_with("/api/v1/platform/entitlement-requests")
}

/// Data export (`GET /api/v1/platform/imports/entities/{entity}/export`): always allowed, so an
/// institution can take its data out whatever its licence (no data hostage).
pub fn is_export(path: &str, unsafe_method: bool) -> bool {
    !unsafe_method && path.starts_with("/api/v1/platform/imports/") && path.ends_with("/export")
}

/// The request as the gate sees it.
#[derive(Clone, Copy, Debug)]
pub struct RequestFacts<'a> {
    pub path: &'a str,
    /// POST / PUT / PATCH / DELETE.
    pub unsafe_method: bool,
    /// The caller administers the tenant's platform (holds `platform.entitlement.manage`).
    pub platform_admin: bool,
}

/// Outcome of [`decide`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Allow,
    Deny {
        /// One of [`MODULE_INACTIVE`], [`LICENCE_EXPIRED`], [`LICENCE_SUSPENDED`].
        code: &'static str,
        title: &'static str,
        detail: String,
        /// Module the request belongs to, if any.
        module: Option<String>,
    },
}

/// The access rule (see the module docs).
pub fn decide(reg: &Registry, ent: &EffectiveEntitlement, req: &RequestFacts<'_>) -> Access {
    if is_always_allowed(req.path) || is_export(req.path, req.unsafe_method) {
        return Access::Allow;
    }
    let module = reg.classify(req.path);
    if ent.status == LicenceStatus::Suspended {
        if req.platform_admin && req.path.starts_with("/api/v1/platform/") {
            return Access::Allow;
        }
        return Access::Deny {
            code: LICENCE_SUSPENDED,
            title: "Licence suspended",
            detail: "this institution's SutraERP licence is suspended: only platform \
                     administration and data export are available until it is reinstated"
                .into(),
            module: module.map(str::to_owned),
        };
    }
    if let Some(m) = module {
        if !ent.has_module(m) {
            let name = reg.module(m).map_or(m, |s| s.name.as_str());
            return Access::Deny {
                code: MODULE_INACTIVE,
                title: "Module not active",
                detail: format!(
                    "the {name} module (`{m}`) is not active for this institution; \
                     a platform administrator can activate it"
                ),
                module: Some(m.to_owned()),
            };
        }
    }
    if ent.status == LicenceStatus::Expired && req.unsafe_method {
        return Access::Deny {
            code: LICENCE_EXPIRED,
            title: "Licence expired",
            detail: "this institution's SutraERP licence has expired: your data stays readable \
                     and exportable, changes need a renewed licence"
                .into(),
            module: module.map(str::to_owned),
        };
    }
    Access::Allow
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn reg() -> &'static Registry {
        registry().unwrap()
    }

    fn t(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn ent(package: Option<&str>, add_ons: &[&str], status: LicenceStatus) -> TenantEntitlement {
        TenantEntitlement {
            package: package.map(str::to_owned),
            add_on_modules: add_ons.iter().map(|s| (*s).to_owned()).collect(),
            status,
            valid_from: t(2026, 1, 1),
            valid_until: Some(t(2026, 12, 31)),
            grace_days: 15,
            version: 1,
            origin: EntitlementOrigin::Stored,
        }
    }

    #[test]
    fn the_embedded_registry_is_valid_and_complete() {
        let r = reg();
        let keys: Vec<&str> = r.modules.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "students",
                "admissions",
                "academics",
                "attendance",
                "examinations",
                "fees",
                "hr",
                "payroll",
                "hostel",
                "transport",
                "library",
                "procurement",
                "inventory",
                "assets",
                "reporting"
            ]
        );
        let pkgs: Vec<&str> = r.packages.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            pkgs,
            [
                "core_academic",
                "academic_plus",
                "institution_operations",
                "complete_erp"
            ]
        );
        // complete_erp holds every activatable module; tiers are supersets of each other.
        let complete: BTreeSet<&str> = r
            .package("complete_erp")
            .unwrap()
            .modules
            .iter()
            .map(String::as_str)
            .collect();
        for m in r.modules.iter().filter(|m| m.activatable) {
            assert!(complete.contains(m.key.as_str()), "{}", m.key);
        }
        for w in r.packages.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            assert!(
                a.modules.iter().all(|m| b.modules.contains(m)),
                "{} ⊄ {}",
                a.key,
                b.key
            );
        }
        assert_eq!(r.default_package, "complete_erp");
        // Every module maps onto a crate of this workspace.
        for m in &r.modules {
            assert!(m.crate_name.starts_with("sutra-"), "{}", m.key);
        }
    }

    #[test]
    fn invalid_registries_are_rejected() {
        let bad = REGISTRY_JSON.replace(
            "\"key\": \"payroll\", \"name\": \"Payroll\", \"crate\": \"sutra-people\", \"activatable\": true, \"requires\": [\"hr\"]",
            "\"key\": \"payroll\", \"name\": \"Payroll\", \"crate\": \"sutra-people\", \"activatable\": true, \"requires\": [\"nonexistent\"]",
        );
        assert_ne!(bad, REGISTRY_JSON, "fixture must change the document");
        let e = Registry::parse(&bad).unwrap_err();
        assert!(e.contains("requires unknown `nonexistent`"), "{e}");
        assert!(Registry::parse("{}").is_err());
    }

    #[test]
    fn import_entities_map_onto_their_modules() {
        let r = reg();
        assert_eq!(r.import_module("students"), Some("students"));
        assert_eq!(r.import_module("historical_results"), Some("students"));
        assert_eq!(r.import_module("staff"), Some("hr"));
        assert_eq!(r.import_module("fee_heads"), Some("fees"));
        assert_eq!(r.import_module("opening_balances"), Some("fees"));
        for core in ["campuses", "org_units", "programmes", "curricula"] {
            assert_eq!(r.import_module(core), None, "{core} is institution set-up");
        }
        let bad = REGISTRY_JSON.replace("\"staff\": \"hr\"", "\"staff\": \"humans\"");
        assert_ne!(bad, REGISTRY_JSON, "fixture must change the document");
        let e = Registry::parse(&bad).unwrap_err();
        assert!(
            e.contains("imports.staff names unknown module `humans`"),
            "{e}"
        );
        let err = module_inactive("fees");
        assert_eq!(err.code(), MODULE_INACTIVE);
        assert!(err.to_string().contains("Fees & finance"), "{err}");
    }

    #[test]
    fn dependency_validation_rejects_incomplete_activations() {
        let r = reg();
        assert!(r.resolve("core_academic", &[]).is_ok());
        assert!(r
            .resolve("core_academic", &["hr".into(), "payroll".into()])
            .is_ok());
        let e = r.resolve("core_academic", &["payroll".into()]).unwrap_err();
        assert_eq!(e[0].code, "dependency_missing");
        assert!(e[0].message.contains("`payroll` requires `hr`"), "{e:?}");
        let e = r
            .resolve("core_academic", &["inventory".into()])
            .unwrap_err();
        assert!(e[0].message.contains("`inventory` requires `procurement`"));
        let e = r.resolve("gold", &[]).unwrap_err();
        assert_eq!(e[0].code, "unknown_package");
        let e = r.resolve("complete_erp", &["teleport".into()]).unwrap_err();
        assert_eq!(e[0].code, "unknown_module");
        // examinations needs academics + students; fees needs students.
        let set: BTreeSet<String> = ["examinations".to_owned()].into();
        let missing = r.missing_dependencies(&set);
        assert!(missing.contains(&("examinations".into(), "academics".into())));
        assert!(missing.contains(&("examinations".into(), "students".into())));
        let set: BTreeSet<String> = ["fees".to_owned()].into();
        assert_eq!(
            r.missing_dependencies(&set),
            vec![("fees".to_owned(), "students".to_owned())]
        );
    }

    #[test]
    fn request_paths_classify_onto_modules() {
        let r = reg();
        let id = "0190f3a8-7a54-7c4e-9b1a-1f2e3d4c5b6a";
        for (path, want) in [
            (format!("/api/v1/student/students/{id}"), Some("students")),
            (
                format!("/api/v1/student/institutions/{id}/admissions"),
                Some("admissions"),
            ),
            (
                format!("/api/v1/student/duplicate-cases/{id}/decision"),
                Some("admissions"),
            ),
            (
                format!("/api/v1/academic/sessions/{id}/finalise-attendance"),
                Some("attendance"),
            ),
            (
                "/api/v1/academic/me/attendance".to_owned(),
                Some("attendance"),
            ),
            ("/api/v1/academic/me/library".to_owned(), Some("library")),
            (
                format!("/api/v1/academic/offerings/{id}"),
                Some("academics"),
            ),
            (
                format!("/api/v1/exams/exam-sessions/{id}"),
                Some("examinations"),
            ),
            ("/api/v1/finance/me/payments".to_owned(), Some("fees")),
            (
                format!("/api/v1/people/payroll-runs/{id}/decision"),
                Some("payroll"),
            ),
            (
                format!("/api/v1/people/engagements/{id}/salary-assignments"),
                Some("payroll"),
            ),
            ("/api/v1/people/me/payslips".to_owned(), Some("payroll")),
            (
                format!("/api/v1/people/staff/{id}/leave-balances"),
                Some("hr"),
            ),
            (
                format!("/api/v1/campus/institutions/{id}/hostel-allocations"),
                Some("hostel"),
            ),
            (
                format!("/api/v1/campus/messes/{id}/allocations"),
                Some("hostel"),
            ),
            (
                "/api/v1/campus/me/transport/pass".to_owned(),
                Some("transport"),
            ),
            (
                format!("/api/v1/campus/trips/{id}/start"),
                Some("transport"),
            ),
            (
                "/api/v1/campus/me/library/loans".to_owned(),
                Some("library"),
            ),
            (
                format!("/api/v1/campus/catalogue-records/{id}"),
                Some("library"),
            ),
            (format!("/api/v1/campus/tickets/{id}"), None),
            (
                format!("/api/v1/campus/institutions/{id}/reservations"),
                None,
            ),
            (
                format!("/api/v1/resources/institutions/{id}/stock-on-hand"),
                Some("inventory"),
            ),
            (
                format!("/api/v1/resources/assets/{id}/retire"),
                Some("assets"),
            ),
            (
                format!("/api/v1/resources/purchase-orders/{id}"),
                Some("procurement"),
            ),
            (
                format!("/api/v1/resources/vendor-invoices/{id}"),
                Some("procurement"),
            ),
            (
                format!("/api/v1/services/institutions/{id}/reports/library"),
                Some("reporting"),
            ),
            (format!("/api/v1/institution/institutions/{id}"), None),
            ("/api/v1/platform/imports".to_owned(), None),
            ("/api/v1/auth/session".to_owned(), None),
            ("/api/v1/notifications".to_owned(), None),
            ("/health/live".to_owned(), None),
        ] {
            assert_eq!(r.classify(&path), want, "{path}");
        }
    }

    /// Every operation of the committed API description classifies without surprise: the gated
    /// areas map every path to a module (campus: core facilities by design), the rest is core.
    #[test]
    fn every_documented_api_path_has_a_module_or_is_core() {
        let spec: serde_json::Value =
            serde_json::from_str(include_str!("../../../openapi/openapi.json")).unwrap();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for path in spec["paths"].as_object().unwrap().keys() {
            let area = path.split('/').nth(3).unwrap_or_default();
            let got = reg().classify(path);
            match area {
                "student" | "academic" | "exams" | "finance" | "people" | "resources"
                | "services" => assert!(got.is_some(), "{path} is ungated"),
                "campus" => {}
                _ => assert_eq!(got, None, "{path} is core"),
            }
            *counts.entry(got.unwrap_or("core").to_owned()).or_default() += 1;
        }
        for m in &reg().modules {
            assert!(
                counts.get(&m.key).copied().unwrap_or(0) > 0,
                "no route for {}",
                m.key
            );
        }
    }

    #[test]
    fn licence_status_moves_active_grace_expired() {
        let from = t(2026, 1, 1);
        let until = Some(t(2026, 12, 31));
        let s = |now| effective_status(LicenceStatus::Active, from, until, 15, now);
        assert_eq!(s(t(2025, 12, 31)), LicenceStatus::Expired, "not yet valid");
        assert_eq!(s(t(2026, 6, 1)), LicenceStatus::Active);
        assert_eq!(s(t(2026, 12, 31)), LicenceStatus::Active);
        assert_eq!(s(t(2027, 1, 10)), LicenceStatus::Grace);
        assert_eq!(s(t(2027, 1, 15)), LicenceStatus::Grace);
        assert_eq!(s(t(2027, 1, 16)), LicenceStatus::Expired);
        assert_eq!(
            effective_status(LicenceStatus::Active, from, None, 0, t(2099, 1, 1)),
            LicenceStatus::Active,
            "perpetual"
        );
        assert_eq!(
            effective_status(LicenceStatus::Suspended, from, until, 15, t(2026, 6, 1)),
            LicenceStatus::Suspended
        );
        assert_eq!(
            effective_status(LicenceStatus::Expired, from, None, 0, t(2026, 6, 1)),
            LicenceStatus::Expired
        );
        assert_eq!(
            effective_status(LicenceStatus::Grace, from, until, 15, t(2026, 6, 1)),
            LicenceStatus::Grace
        );
        for s in LicenceStatus::ALL {
            assert_eq!(LicenceStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(LicenceStatus::parse("ACTIVE"), None);
    }

    fn facts(path: &str, unsafe_method: bool, platform_admin: bool) -> RequestFacts<'_> {
        RequestFacts {
            path,
            unsafe_method,
            platform_admin,
        }
    }

    fn code(a: Access) -> Option<&'static str> {
        match a {
            Access::Allow => None,
            Access::Deny { code, .. } => Some(code),
        }
    }

    #[test]
    fn access_rule_per_licence_state() {
        let r = reg();
        let exam = "/api/v1/exams/exam-sessions";
        let payroll = "/api/v1/people/payroll-runs";
        let export = "/api/v1/platform/imports/entities/students/export";
        let active =
            ent(Some("core_academic"), &[], LicenceStatus::Active).effective(r, t(2026, 6, 1));
        assert_eq!(code(decide(r, &active, &facts(exam, true, false))), None);
        assert_eq!(
            code(decide(r, &active, &facts(payroll, false, true))),
            Some(MODULE_INACTIVE),
            "a role never substitutes for the module"
        );
        assert_eq!(
            code(decide(
                r,
                &active,
                &facts("/api/v1/institution/institutions", true, false)
            )),
            None
        );
        // Grace: full access.
        let grace =
            ent(Some("core_academic"), &[], LicenceStatus::Active).effective(r, t(2027, 1, 5));
        assert_eq!(grace.status, LicenceStatus::Grace);
        assert!(!grace.read_only);
        assert_eq!(grace.days_remaining, Some(10));
        assert_eq!(code(decide(r, &grace, &facts(exam, true, false))), None);
        // Expired: reads + export, no writes; inactive modules stay inactive.
        let expired =
            ent(Some("core_academic"), &[], LicenceStatus::Active).effective(r, t(2027, 2, 1));
        assert!(expired.read_only);
        assert_eq!(code(decide(r, &expired, &facts(exam, false, false))), None);
        assert_eq!(
            code(decide(r, &expired, &facts(exam, true, false))),
            Some(LICENCE_EXPIRED)
        );
        assert_eq!(
            code(decide(r, &expired, &facts(export, false, false))),
            None
        );
        assert_eq!(
            code(decide(
                r,
                &expired,
                &facts("/api/v1/platform/imports", true, true)
            )),
            Some(LICENCE_EXPIRED)
        );
        assert_eq!(
            code(decide(r, &expired, &facts(payroll, false, false))),
            Some(MODULE_INACTIVE)
        );
        assert_eq!(
            code(decide(
                r,
                &expired,
                &facts("/api/v1/auth/logout", true, false)
            )),
            None
        );
        assert_eq!(
            code(decide(
                r,
                &expired,
                &facts("/api/v1/platform/entitlement-requests", true, true)
            )),
            None
        );
        // Suspended: platform admin area + export only.
        let susp =
            ent(Some("complete_erp"), &[], LicenceStatus::Suspended).effective(r, t(2026, 6, 1));
        assert_eq!(
            code(decide(r, &susp, &facts(exam, false, true))),
            Some(LICENCE_SUSPENDED)
        );
        assert_eq!(
            code(decide(
                r,
                &susp,
                &facts("/api/v1/platform/audit/chain", false, false)
            )),
            Some(LICENCE_SUSPENDED)
        );
        assert_eq!(
            code(decide(
                r,
                &susp,
                &facts("/api/v1/platform/audit/chain", false, true)
            )),
            None
        );
        assert_eq!(code(decide(r, &susp, &facts(export, false, false))), None);
        assert_eq!(
            code(decide(
                r,
                &susp,
                &facts("/api/v1/platform/entitlement", false, false)
            )),
            None
        );
        assert_eq!(
            code(decide(r, &susp, &facts("/api/v1/todos", false, false))),
            Some(LICENCE_SUSPENDED)
        );
    }

    #[test]
    fn effective_entitlement_unions_package_and_add_ons_and_drops_unknown_keys() {
        let r = reg();
        let e = ent(
            Some("core_academic"),
            &["hr", "payroll", "warp-drive"],
            LicenceStatus::Active,
        )
        .effective(r, t(2026, 6, 1));
        assert!(e.has_module("payroll") && e.has_module("examinations"));
        assert!(!e.has_module("warp-drive") && !e.has_module("fees"));
        assert_eq!(e.days_remaining, Some(213));
        let none = DefaultEntitlement::parse("none", r)
            .unwrap()
            .entitlement()
            .effective(r, t(2026, 6, 1));
        assert!(none.modules.is_empty());
        assert_eq!(none.origin, EntitlementOrigin::Default);
        let all = DefaultEntitlement::parse("complete_erp", r)
            .unwrap()
            .entitlement()
            .effective(r, t(2026, 6, 1));
        assert_eq!(all.modules.len(), r.modules.len());
        assert_eq!(all.status, LicenceStatus::Active);
        assert!(DefaultEntitlement::parse("gold", r).is_none());
    }

    #[test]
    fn manual_clock_moves() {
        let c = ManualClock::new(t(2026, 1, 1));
        c.advance(Duration::days(2));
        assert_eq!(c.now(), t(2026, 1, 3));
        c.set(t(2030, 1, 1));
        assert_eq!(c.now(), t(2030, 1, 1));
    }
}
