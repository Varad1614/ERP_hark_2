#!/usr/bin/env python3
"""Security checks of the SutraERP Keycloak realms (Part 1 S3 auth, SEC-9D-005 / SEC-P1-AUTH-004).

Usage: check-realm.py <compose realm.json> <production baseline realm.json>

Both realms must carry the same brute-force / OTP / client security settings (production
parity with compose); the production baseline must additionally require TLS, carry a password
policy, https-only redirect URIs and no users or client secrets. Exit 1 on any violation.
"""
import json
import sys

PARITY_KEYS = [
    "registrationAllowed", "bruteForceProtected", "permanentLockout", "failureFactor",
    "waitIncrementSeconds", "maxFailureWaitSeconds", "maxDeltaTimeSeconds",
    "quickLoginCheckMilliSeconds", "minimumQuickLoginWaitSeconds", "otpPolicyType",
    "otpPolicyAlgorithm", "otpPolicyDigits", "otpPolicyPeriod", "otpPolicyLookAheadWindow",
    "otpPolicyCodeReusable", "revokeRefreshToken", "refreshTokenMaxReuse",
    "ssoSessionIdleTimeout", "ssoSessionMaxLifespan", "accessTokenLifespan", "browserFlow",
    "authenticationFlows", "authenticatorConfig", "requiredActions",
]
WEB_PARITY_ATTRS = [
    "pkce.code.challenge.method", "backchannel.logout.session.required", "use.refresh.tokens",
    "acr.loa.map", "minimum.acr.value", "id.token.signed.response.alg",
]
# Part 2 S4-idp: least-privilege realm-management roles of the identity admin client.
IDP_ADMIN_REQUIRED = {"manage-users", "view-users", "query-users", "query-groups"}
IDP_ADMIN_ALLOWED = IDP_ADMIN_REQUIRED | {"query-clients"}
PRIVILEGED = {"administrator", "finance_approver", "exam_controller", "registrar",
              "payroll_approver", "procurement_approver"}

errors = []


def check(cond, msg):
    if not cond:
        errors.append(msg)


def client(realm, cid):
    return next((c for c in realm.get("clients", []) if c.get("clientId") == cid), None)


def common(name, r):
    check(r.get("bruteForceProtected") is True, f"{name}: bruteForceProtected must be true")
    check(0 < r.get("failureFactor", 30) <= 5, f"{name}: failureFactor must be 1..5 (Keycloak default 30)")
    check(r.get("waitIncrementSeconds", 0) >= 60, f"{name}: waitIncrementSeconds >= 60")
    check(r.get("maxFailureWaitSeconds", 0) >= 900, f"{name}: maxFailureWaitSeconds >= 900")
    check(r.get("quickLoginCheckMilliSeconds", 0) > 0, f"{name}: quick-login check on")
    check(r.get("otpPolicyCodeReusable") is False, f"{name}: OTP codes must be single use")
    check(r.get("registrationAllowed") is False, f"{name}: self-registration must be off")
    web = client(r, "sutra-web")
    check(web is not None, f"{name}: client sutra-web missing")
    if web:
        a = web.get("attributes", {})
        check(web.get("publicClient") is False, f"{name}: sutra-web must be confidential")
        check(web.get("standardFlowEnabled") is True, f"{name}: sutra-web uses the code flow")
        check(web.get("implicitFlowEnabled") is not True, f"{name}: no implicit flow")
        check(web.get("directAccessGrantsEnabled") is False, f"{name}: no ROPC (direct grants)")
        check(a.get("pkce.code.challenge.method") == "S256", f"{name}: PKCE S256 required")
        check(a.get("use.refresh.tokens") == "false", f"{name}: no refresh tokens for the BFF")
        check(a.get("backchannel.logout.session.required") == "true", f"{name}: back-channel logout with sid")
        check(bool(a.get("backchannel.logout.url")), f"{name}: back-channel logout URL set")
        check(a.get("id.token.signed.response.alg") == "RS256",
              f"{name}: ID token alg must match SUTRA_OIDC_ALGS default RS256")
        for u in web.get("redirectUris", []):
            check("*" not in u, f"{name}: wildcard redirect URI {u}")
        check("*" not in web.get("webOrigins", []), f"{name}: wildcard web origin")
    api = client(r, "sutra-api")
    if api:
        check(api.get("standardFlowEnabled") is False and api.get("directAccessGrantsEnabled") is False,
              f"{name}: sutra-api has no browser / password flows")
    idp = client(r, "sutra-idp-admin")
    check(idp is not None, f"{name}: client sutra-idp-admin (identity provisioning) missing")
    if idp:
        check(idp.get("publicClient") is False and idp.get("serviceAccountsEnabled") is True,
              f"{name}: sutra-idp-admin must be a confidential service-account client")
        check(not idp.get("standardFlowEnabled") and not idp.get("implicitFlowEnabled")
              and not idp.get("directAccessGrantsEnabled"),
              f"{name}: sutra-idp-admin has no browser / password flows")
    for m in (web or {}).get("protocolMappers", []):
        if m.get("config", {}).get("claim.name") == "tenant_id":
            check(m.get("protocolMapper") == "oidc-usermodel-attribute-mapper",
                  f"{name}: tenant_id must come from the (group) attribute, not {m.get('protocolMapper')}")
    flows = json.dumps(r.get("authenticationFlows", []))
    check("auth-otp-form" in flows and "amr-otp" in flows,
          f"{name}: browser flow must include the OTP form with amr=otp")
    roles = {x["name"] for x in r.get("roles", {}).get("realm", [])}
    check(PRIVILEGED <= roles, f"{name}: privileged realm roles missing: {PRIVILEGED - roles}")


def main():
    dev_path, prod_path = sys.argv[1], sys.argv[2]
    dev = json.load(open(dev_path))
    prod = json.load(open(prod_path))
    common("compose", dev)
    common("production", prod)
    # compose: every privileged persona must enrol TOTP at first sign-in.
    for u in dev.get("users", []):
        if PRIVILEGED & set(u.get("realmRoles", [])):
            check("CONFIGURE_TOTP" in u.get("requiredActions", []),
                  f"compose: privileged user {u['username']} needs CONFIGURE_TOTP")
    # compose: the identity admin service account holds exactly the least-privilege roles.
    sa = next((u for u in dev.get("users", []) if u.get("serviceAccountClientId") == "sutra-idp-admin"), None)
    check(sa is not None, "compose: service account of sutra-idp-admin missing")
    if sa:
        held = set(sa.get("clientRoles", {}).get("realm-management", []))
        check(IDP_ADMIN_REQUIRED <= held, f"compose: sutra-idp-admin lacks {sorted(IDP_ADMIN_REQUIRED - held)}")
        check(held <= IDP_ADMIN_ALLOWED, f"compose: sutra-idp-admin over-privileged {sorted(held - IDP_ADMIN_ALLOWED)}")
        check(not set(sa.get("realmRoles", [])) & (PRIVILEGED | {"faculty", "student"}),
              "compose: sutra-idp-admin service account holds SutraERP roles")
        check(len(sa.get("clientRoles", {})) == 1, "compose: sutra-idp-admin holds roles of other clients")
    # production specifics
    check(prod.get("sslRequired") == "all", "production: sslRequired must be 'all'")
    check("length(" in prod.get("passwordPolicy", ""), "production: password policy with a minimum length")
    check(not prod.get("users"), "production: the baseline carries no users")
    for c in prod.get("clients", []):
        check("secret" not in c, f"production: client {c.get('clientId')} carries a secret")
    web = client(prod, "sutra-web") or {}
    a = web.get("attributes", {})
    for u in web.get("redirectUris", []) + a.get("post.logout.redirect.uris", "").split("##") + [a.get("backchannel.logout.url", "")]:
        if u:
            check(u.startswith("https://"), f"production: non-https URI {u}")
    for m in web.get("protocolMappers", []):
        check(m.get("protocolMapper") != "oidc-hardcoded-claim-mapper",
              f"production: hardcoded claim mapper {m.get('name')}")
    # parity
    for k in PARITY_KEYS:
        check(dev.get(k) == prod.get(k), f"parity: realm setting {k} differs between compose and production")
    dweb = client(dev, "sutra-web") or {}
    for k in WEB_PARITY_ATTRS:
        check(dweb.get("attributes", {}).get(k) == a.get(k), f"parity: sutra-web attribute {k} differs")
    for k in ["publicClient", "standardFlowEnabled", "implicitFlowEnabled", "directAccessGrantsEnabled", "frontchannelLogout"]:
        check(dweb.get(k) == web.get(k), f"parity: sutra-web {k} differs")
    if errors:
        for e in errors:
            print(f"FAIL {e}", file=sys.stderr)
        sys.exit(1)
    print(f"ok keycloak realms ({dev_path}, {prod_path}): brute force, OTP, client flags, identity admin client, parity")


if __name__ == "__main__":
    main()
