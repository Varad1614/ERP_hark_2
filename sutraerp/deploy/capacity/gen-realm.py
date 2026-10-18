#!/usr/bin/env python3
"""Part 2 · S5-capacity: the Keycloak realm of the capacity stack (deploy/capacity/README.md).

Starts from deploy/keycloak/realm-sutra-dev.json (same flows, mappers, roles, brute-force and OTP
policy, the least-privilege `sutra-idp-admin` service account, the tenant group) and changes only
what a production install changes:

* the sign-in client `sutra-web`: https redirect / post-logout URIs and web origin of
  https://erp.<domain>, a random client secret; `sutra-api` / `sutra-idp-admin`: random secrets;
* every seeded persona (administrators, checkers, faculty, student of the dev realm) gets the
  per-run random password `SEED_PASSWORD` (administrators still enrol TOTP on first sign-in);
* the synthetic staff of the capacity institution, with fixed ids (= the OIDC `sub` the ERP
  stores as subject) in the dev tenant group:
    - `FACULTY` teachers `cap.faculty.NNN@<domain>` (realm role `faculty`, password only — faculty
      is not a privileged role, so no second factor), and
    - `OFFICE` office staff `cap.office.NNN@<domain>` (realm role `administrator`: student
      search, counter fee collection, reports) with a pre-enrolled TOTP credential (privileged
      roles need MFA in production; the k6 script computes RFC 6238 codes from the secret).
Students are NOT in the realm: they are imported into SutraERP and provisioned into Keycloak by
the API's identity jobs (S4-idp), exactly like a migrated college.

Usage: gen-realm.py <out-realm.json> <out-staff.json>   (env: CAP_DOMAIN, OIDC_CLIENT_SECRET,
IDP_CLIENT_SECRET, API_CLIENT_SECRET, SEED_PASSWORD, CAP_FACULTY=100, CAP_OFFICE=40)
"""
import json
import os
import secrets
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "..", "keycloak", "realm-sutra-dev.json")
TENANT = "0192f5a0-7c3e-7000-8000-000000000001"


def main() -> None:
    out_realm, out_staff = sys.argv[1], sys.argv[2]
    domain = os.environ.get("CAP_DOMAIN", "sutra.test")
    password = os.environ["SEED_PASSWORD"]
    n_faculty = int(os.environ.get("CAP_FACULTY", "100"))
    n_office = int(os.environ.get("CAP_OFFICE", "40"))
    erp = f"https://erp.{domain}"
    realm = json.load(open(SRC))
    group = f"/sutra-tenant-{TENANT}"
    assert any(g["path"] == group for g in realm["groups"]), "dev tenant group missing"

    for c in realm["clients"]:
        if c["clientId"] == "sutra-web":
            c["secret"] = os.environ["OIDC_CLIENT_SECRET"]
            c["redirectUris"] = [f"{erp}/api/v1/auth/oidc/callback"]
            c["webOrigins"] = [erp]
            c["attributes"]["post.logout.redirect.uris"] = f"{erp}/sign-in"
        elif c["clientId"] == "sutra-idp-admin":
            c["secret"] = os.environ["IDP_CLIENT_SECRET"]
        elif c["clientId"] == "sutra-api":
            c["secret"] = os.environ["API_CLIENT_SECRET"]
    for u in realm["users"]:
        for cred in u.get("credentials", []):
            if cred.get("type") == "password":
                cred["value"] = password

    staff = {"faculty": [], "office": [], "tenantId": TENANT}
    ns = uuid.UUID("5a7e7a00-cafe-4000-8000-00000000ca90")
    for i in range(1, n_faculty + 1):
        login = f"cap.faculty.{i:03d}@{domain}"
        uid = str(uuid.uuid5(ns, login))
        realm["users"].append({
            "id": uid, "username": login, "email": login, "emailVerified": True, "enabled": True,
            "firstName": f"Faculty{i:03d}", "lastName": "Capacity",
            "realmRoles": ["faculty"], "groups": [group],
            "credentials": [{"type": "password", "value": password, "temporary": False}],
        })
        staff["faculty"].append({"login": login, "subject": uid, "name": f"Prof. Faculty{i:03d} Capacity"})
    for i in range(1, n_office + 1):
        login = f"cap.office.{i:03d}@{domain}"
        uid = str(uuid.uuid5(ns, login))
        # Keycloak's TOTP key is the UTF-8 bytes of this string (as on the configure page).
        otp = secrets.token_hex(10)
        realm["users"].append({
            "id": uid, "username": login, "email": login, "emailVerified": True, "enabled": True,
            "firstName": f"Office{i:03d}", "lastName": "Capacity",
            "realmRoles": ["administrator"], "groups": [group],
            "credentials": [
                {"type": "password", "value": password, "temporary": False},
                {"type": "otp", "userLabel": "capacity", "secretData": json.dumps({"value": otp}),
                 "credentialData": json.dumps({"subType": "totp", "digits": 6, "counter": 0,
                                               "period": 30, "algorithm": "HmacSHA1"})},
            ],
        })
        staff["office"].append({"login": login, "subject": uid, "otp": otp})
    json.dump(realm, open(out_realm, "w"), indent=1)
    json.dump(staff, open(out_staff, "w"), indent=1)
    print(f"ok realm {out_realm}: {n_faculty} faculty, {n_office} office staff (TOTP), redirect {erp}")


if __name__ == "__main__":
    main()
