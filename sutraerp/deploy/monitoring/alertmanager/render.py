#!/usr/bin/env python3
"""Render Alertmanager's configuration from the monitoring env file (Part 2 S5-monitor).

    python3 render.py --env /etc/sutraerp/monitoring.env --out alertmanager.yml

Receivers are TEMPLATES: the college's SMTP relay, mailbox, chat webhook and SMS gateway are not
known yet (BLOCKED_EXTERNAL). Every receiver whose settings are empty is rendered without that
integration, so the stack starts with whatever is configured; `--require-delivery` refuses to
render a production config in which no alert would reach a person.

Routing: SutraWatchdog -> deadman (heartbeat); critical -> email + SMS + chat (repeat 1 h);
warning -> email + chat (repeat 4 h); info -> chat. PostgreSQL / host down inhibit the alerts they
cause. Standard library only. The output holds secrets (SMTP password): it is written mode 0600.
"""
import argparse
import json
import os
import sys

CHAT_KINDS = ("webhook", "slack", "msteamsv2", "discord")


def read_env(path):
    env = {}
    if path:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip().strip('"').strip("'")
    for k, v in os.environ.items():
        if k.startswith("SUTRA_AM_"):
            env.setdefault(k, v)
    return env


def render(env):
    g = lambda k, d="": env.get(k, d) or d  # noqa: E731
    cfg = {"global": {"resolve_timeout": "5m"}, "templates": ["/etc/alertmanager/templates/*.tmpl"]}
    smarthost, mail_to = g("SUTRA_AM_SMTP_SMARTHOST"), g("SUTRA_AM_EMAIL_TO")
    email = None
    if smarthost and mail_to:
        cfg["global"].update({
            "smtp_smarthost": smarthost,
            "smtp_from": g("SUTRA_AM_SMTP_FROM", "sutraerp-alerts@localhost"),
            "smtp_require_tls": g("SUTRA_AM_SMTP_REQUIRE_TLS", "true").lower() == "true",
        })
        if g("SUTRA_AM_SMTP_USERNAME"):
            cfg["global"]["smtp_auth_username"] = g("SUTRA_AM_SMTP_USERNAME")
            cfg["global"]["smtp_auth_password"] = g("SUTRA_AM_SMTP_PASSWORD")
        email = {"to": mail_to, "send_resolved": True,
                 "headers": {"Subject": '{{ template "sutra.subject" . }}'},
                 "text": '{{ template "sutra.text" . }}'}
    chat, kind = g("SUTRA_AM_CHAT_URL"), g("SUTRA_AM_CHAT_KIND", "webhook")
    if kind not in CHAT_KINDS:
        sys.exit(f"SUTRA_AM_CHAT_KIND must be one of {CHAT_KINDS}")
    chat_cfg = None
    if chat:
        chat_cfg = {
            "webhook": ("webhook_configs", {"url": chat, "send_resolved": True}),
            "slack": ("slack_configs", {"api_url": chat, "send_resolved": True,
                                        "title": '{{ template "sutra.subject" . }}',
                                        "text": '{{ template "sutra.text" . }}'}),
            "msteamsv2": ("msteamsv2_configs", {"webhook_url": chat, "send_resolved": True,
                                                "title": '{{ template "sutra.subject" . }}',
                                                "text": '{{ template "sutra.text" . }}'}),
            "discord": ("discord_configs", {"webhook_url": chat, "send_resolved": True,
                                            "title": '{{ template "sutra.subject" . }}',
                                            "message": '{{ template "sutra.text" . }}'}),
        }[kind]
    sms = g("SUTRA_AM_SMS_WEBHOOK_URL")
    deadman = g("SUTRA_AM_DEADMAN_URL")

    def receiver(name, use_email, use_chat, use_sms):
        r = {"name": name}
        if use_email and email:
            r.setdefault("email_configs", []).append(dict(email))
        if use_chat and chat_cfg:
            r.setdefault(chat_cfg[0], []).append(dict(chat_cfg[1]))
        if use_sms and sms:
            # SMS gateway adapter: receives the standard Alertmanager webhook JSON.
            r.setdefault("webhook_configs", []).append({"url": sms, "send_resolved": False,
                                                        "max_alerts": 5})
        return r

    receivers = [receiver("ops-critical", True, True, True), receiver("ops-warning", True, True, False),
                 receiver("ops-info", False, True, False), {"name": "deadman"}]
    if deadman:
        receivers[-1]["webhook_configs"] = [{"url": deadman, "send_resolved": False}]
    cfg["receivers"] = receivers
    cfg["route"] = {
        "receiver": "ops-warning",
        "group_by": ["alertname", "severity"],
        "group_wait": g("SUTRA_AM_GROUP_WAIT", "30s"),
        "group_interval": "5m",
        "repeat_interval": "4h",
        "routes": [
            {"receiver": "deadman", "matchers": ['alertname="SutraWatchdog"'], "group_wait": "0s",
             "group_interval": "1m", "repeat_interval": "5m"},
            {"receiver": "ops-critical", "matchers": ['severity="critical"'], "repeat_interval": "1h"},
            {"receiver": "ops-warning", "matchers": ['severity="warning"']},
            {"receiver": "ops-info", "matchers": ['severity="info"']},
        ],
    }
    cfg["inhibit_rules"] = [
        # PostgreSQL down explains the database, backup and readiness symptoms.
        {"source_matchers": ['alertname="SutraPostgresDown"'],
         "target_matchers": ['alertname=~"SutraPostgres(ConnectionsHigh|LongTransaction|SlowQueries|TableBloat|'
                             'Deadlocks|ReplicationLag)|SutraWalArchive.*|SutraDbPoolSaturated|SutraApiHighErrorRate"']},
        # A host that is down explains everything that runs on it.
        {"source_matchers": ['alertname="SutraHostDown"'],
         "target_matchers": ['alertname=~"SutraHost(DiskSpaceLow|DiskSpaceCritical|DiskFillingUp|CpuHigh|MemoryLow)"']},
        # Critical disk space supersedes the low-space warning for the same filesystem.
        {"source_matchers": ['alertname="SutraHostDiskSpaceCritical"'],
         "target_matchers": ['alertname="SutraHostDiskSpaceLow"'], "equal": ["instance", "mountpoint"]},
    ]
    delivers = bool(email or chat_cfg or sms)
    return cfg, delivers


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--env", help="monitoring env file (KEY=VALUE lines)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--require-delivery", action="store_true",
                    help="fail when no email / chat / SMS receiver is configured")
    a = ap.parse_args()
    cfg, delivers = render(read_env(a.env))
    if a.require_delivery and not delivers:
        sys.exit("no alert delivery configured (SUTRA_AM_SMTP_SMARTHOST + SUTRA_AM_EMAIL_TO, "
                 "SUTRA_AM_CHAT_URL or SUTRA_AM_SMS_WEBHOOK_URL)")
    fd = os.open(a.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write("# GENERATED by deploy/monitoring/alertmanager/render.py — do not edit; holds secrets.\n")
        # JSON is valid YAML: no PyYAML needed on the server.
        json.dump(cfg, f, indent=2)
        f.write("\n")
    print(f"wrote {a.out} ({'with' if delivers else 'WITHOUT'} a delivery receiver)")


if __name__ == "__main__":
    main()
