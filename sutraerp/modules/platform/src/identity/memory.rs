//! In-memory [`IdentityProvisioningPort`] for tests and no-IdP development
//! (`SUTRA_IDP_PROVISIONING=memory`, refused in production). It mimics Keycloak's observable
//! rules: realm-wide unique usernames / e-mails (`409`), required actions as a set, groups by
//! path, and can be switched "down" to exercise the retry path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use sutra_contracts::platform::identity::{
    IdentityProvisioningPort, IdpGroup, IdpUser, NewIdpUser, RequiredAction, ScopeGroup,
};
use sutra_kernel::error::KernelError;

/// One stored user.
#[derive(Clone, Debug, Default)]
pub struct MemoryUser {
    pub username: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub required_actions: BTreeSet<String>,
    pub realm_roles: BTreeSet<String>,
    /// client id → roles.
    pub client_roles: BTreeMap<String, BTreeSet<String>>,
    /// group ids.
    pub groups: BTreeSet<String>,
    /// Number of `logout` calls (sessions ended).
    pub logouts: u32,
    /// Actions e-mails sent.
    pub action_emails: Vec<Vec<String>>,
}

#[derive(Default)]
struct State {
    users: BTreeMap<String, MemoryUser>,
    /// group id → path.
    groups: BTreeMap<String, String>,
}

/// In-memory identity provider.
#[derive(Default)]
pub struct MemoryIdentityProvider {
    state: Mutex<State>,
    down: AtomicBool,
    seq: AtomicU64,
    /// Calls made (any method), for tests.
    pub calls: AtomicU64,
}

impl MemoryIdentityProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulates the IdP being unreachable (`true`) or back (`false`).
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Snapshot of one user.
    pub fn snapshot(&self, id: &str) -> Option<MemoryUser> {
        self.lock().ok()?.users.get(id).cloned()
    }

    /// Number of users.
    pub fn user_count(&self) -> usize {
        self.lock().map(|s| s.users.len()).unwrap_or(0)
    }

    /// Adds a user directly (an account that pre-exists SutraERP).
    pub fn seed_user(&self, username: &str, email: Option<&str>) -> String {
        let id = self.next_id("u");
        if let Ok(mut s) = self.lock() {
            s.users.insert(
                id.clone(),
                MemoryUser {
                    username: username.to_owned(),
                    email: email.map(str::to_owned),
                    enabled: true,
                    ..MemoryUser::default()
                },
            );
        }
        id
    }

    /// Path of a group id.
    pub fn group_path(&self, id: &str) -> Option<String> {
        self.lock().ok()?.groups.get(id).cloned()
    }

    fn next_id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{:08}-{}",
            self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            uuid::Uuid::now_v7().simple()
        )
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, KernelError> {
        self.state
            .lock()
            .map_err(|_| KernelError::internal_msg("memory identity provider poisoned"))
    }

    fn gate(&self) -> Result<(), KernelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.down.load(Ordering::SeqCst) {
            return Err(KernelError::Unavailable(
                "identity provider unreachable (simulated)".into(),
            ));
        }
        Ok(())
    }

    fn with_user<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut MemoryUser) -> T,
    ) -> Result<T, KernelError> {
        self.gate()?;
        let mut s = self.lock()?;
        let u = s.users.get_mut(id).ok_or(KernelError::NotFound {
            entity: "identity provider user",
        })?;
        Ok(f(u))
    }

    fn view(id: &str, u: &MemoryUser) -> IdpUser {
        IdpUser {
            id: id.to_owned(),
            username: u.username.clone(),
            email: u.email.clone(),
            enabled: u.enabled,
            email_verified: false,
            required_actions: u.required_actions.iter().cloned().collect(),
        }
    }
}

#[async_trait::async_trait]
impl IdentityProvisioningPort for MemoryIdentityProvider {
    fn kind(&self) -> &'static str {
        "memory"
    }

    async fn check(&self) -> Result<(), KernelError> {
        self.gate()
    }

    async fn user(&self, id: &str) -> Result<Option<IdpUser>, KernelError> {
        self.gate()?;
        Ok(self.lock()?.users.get(id).map(|u| Self::view(id, u)))
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<IdpUser>, KernelError> {
        self.gate()?;
        Ok(self
            .lock()?
            .users
            .iter()
            .find(|(_, u)| u.username.eq_ignore_ascii_case(username))
            .map(|(id, u)| Self::view(id, u)))
    }

    async fn find_by_email(&self, email: &str) -> Result<Vec<IdpUser>, KernelError> {
        self.gate()?;
        Ok(self
            .lock()?
            .users
            .iter()
            .filter(|(_, u)| {
                u.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(email))
            })
            .map(|(id, u)| Self::view(id, u))
            .collect())
    }

    async fn create_user(&self, user: &NewIdpUser) -> Result<String, KernelError> {
        self.gate()?;
        let mut s = self.lock()?;
        let taken = s.users.values().any(|u| {
            u.username.eq_ignore_ascii_case(&user.username)
                || matches!((&u.email, &user.email), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
        });
        if taken {
            return Err(KernelError::Conflict(
                "identity provider: User exists with same username or email".into(),
            ));
        }
        let id = self.next_id("u");
        s.users.insert(
            id.clone(),
            MemoryUser {
                username: user.username.to_ascii_lowercase(),
                email: user.email.clone(),
                enabled: user.enabled,
                required_actions: user
                    .required_actions
                    .iter()
                    .map(|a| a.keycloak().to_owned())
                    .collect(),
                ..MemoryUser::default()
            },
        );
        Ok(id)
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), KernelError> {
        self.with_user(id, |u| u.enabled = enabled)
    }

    async fn add_required_actions(
        &self,
        id: &str,
        actions: &[RequiredAction],
        send_email: bool,
    ) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            for a in actions {
                u.required_actions.insert(a.keycloak().to_owned());
            }
            if send_email {
                u.action_emails
                    .push(actions.iter().map(|a| a.keycloak().to_owned()).collect());
            }
        })
    }

    async fn logout(&self, id: &str) -> Result<(), KernelError> {
        self.with_user(id, |u| u.logouts += 1)
    }

    async fn realm_roles(&self, id: &str) -> Result<Vec<String>, KernelError> {
        self.with_user(id, |u| u.realm_roles.iter().cloned().collect())
    }

    async fn add_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError> {
        self.with_user(id, |u| u.realm_roles.extend(roles.iter().cloned()))
    }

    async fn remove_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            for r in roles {
                u.realm_roles.remove(r);
            }
        })
    }

    async fn client_roles(&self, id: &str, client_id: &str) -> Result<Vec<String>, KernelError> {
        self.with_user(id, |u| {
            u.client_roles
                .get(client_id)
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default()
        })
    }

    async fn add_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            u.client_roles
                .entry(client_id.to_owned())
                .or_default()
                .extend(roles.iter().cloned());
        })
    }

    async fn remove_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            if let Some(set) = u.client_roles.get_mut(client_id) {
                for r in roles {
                    set.remove(r);
                }
            }
        })
    }

    async fn groups(&self, id: &str) -> Result<Vec<IdpGroup>, KernelError> {
        self.gate()?;
        let s = self.lock()?;
        let u = s.users.get(id).ok_or(KernelError::NotFound {
            entity: "identity provider user",
        })?;
        Ok(u.groups
            .iter()
            .filter_map(|g| {
                s.groups.get(g).map(|p| IdpGroup {
                    id: g.clone(),
                    path: p.clone(),
                })
            })
            .collect())
    }

    async fn ensure_scope_group(&self, group: &ScopeGroup) -> Result<String, KernelError> {
        self.gate()?;
        let path = group.path();
        let mut s = self.lock()?;
        if let Some((id, _)) = s.groups.iter().find(|(_, p)| **p == path) {
            return Ok(id.clone());
        }
        let id = self.next_id("g");
        s.groups.insert(id.clone(), path);
        Ok(id)
    }

    async fn group_by_path(&self, path: &str) -> Result<Option<String>, KernelError> {
        self.gate()?;
        Ok(self
            .lock()?
            .groups
            .iter()
            .find(|(_, p)| p.as_str() == path)
            .map(|(id, _)| id.clone()))
    }

    async fn join_group(&self, id: &str, group_id: &str) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            u.groups.insert(group_id.to_owned());
        })
    }

    async fn leave_group(&self, id: &str, group_id: &str) -> Result<(), KernelError> {
        self.with_user(id, |u| {
            u.groups.remove(group_id);
        })
    }
}
