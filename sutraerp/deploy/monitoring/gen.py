#!/usr/bin/env python3
"""Part 2 S5-monitor: on-prem monitoring rules, promtool tests and Grafana dashboards.

Run from deploy/monitoring/:  python3 gen.py
Generates (do not edit the outputs by hand; CI fails when they are stale):
  prometheus/rules/sutra-onprem.rules.yaml             alert rules (groups per area)
  prometheus/rules/tests/sutra-onprem.rules.test.yaml  promtool unit tests (fire + quiet per alert)
  grafana/dashboards/sutra-*.json                       on-prem dashboards (postgres, security,
                                                        queues & integrations, host & backups)
The API RED / pool / outbox / audit alerts and dashboards of slice 9A stay in
deploy/observability (alerts/sutra.rules.yaml, dashboards/) and are loaded alongside these.
Runbooks: docs/readiness/MONITORING_AND_SUPPORT.md (anchor = lower-case alert name).
"""
import json
import os

import yaml

RUNBOOK = ("https://github.com/Varad1614/ERP_hark_2/blob/erp_merge/sutraerp/docs/readiness/"
           "MONITORING_AND_SUPPORT.md#")
FS = 'fstype!~"tmpfs|overlay|squashfs|nsfs|ramfs|devtmpfs"'

ALERTS = []  # (group, rule dict, test spec)


def alert(group, name, expr, for_, severity, summary, description, fire, quiet):
    """fire / quiet: (input_series, eval_time, exp_labels or None for quiet, interval)."""
    rule = {"alert": name, "expr": expr}
    if for_:
        rule["for"] = for_
    rule["labels"] = {"severity": severity}
    rule["annotations"] = {"summary": summary, "description": description,
                           "runbook_url": RUNBOOK + name.lower()}
    ALERTS.append((group, rule, fire, quiet))


def s(metric, labels, values):
    lab = ",".join(f'{k}="{v}"' for k, v in labels.items())
    return {"series": f"{metric}{{{lab}}}", "values": values}


API = {"job": "sutra-api", "instance": "api-0:9464"}
PG = {"job": "postgres", "instance": "postgres-exporter:9187"}
NODE = {"job": "node", "instance": "host:9100"}

# ---------------------------------------------------------------- API (beyond slice 9A's rules)
alert("sutra-api", "SutraApiDown", 'up{job="sutra-api"} == 0', "2m", "critical",
      "SutraERP API is not being scraped (down or unreachable)",
      "Prometheus cannot scrape the API metrics listener for 2 minutes: the API process is down, "
      "restarting in a loop, or the metrics port is blocked.",
      ([s("up", API, "0x10")], "5m", dict(API)), ([s("up", API, "1x10")], "5m", None))
alert("sutra-api", "SutraApiSaturated", "sum(sutra_http_requests_in_flight) > 100", "5m", "warning",
      "SutraERP API has more than 100 requests in flight",
      "More than 100 requests have been in flight for 5 minutes: the API or the database is not "
      "keeping up (check latency, the DB pool and host CPU).",
      ([s("sutra_http_requests_in_flight", API, "150x20")], "10m", {}),
      ([s("sutra_http_requests_in_flight", API, "5x20")], "10m", None))
alert("sutra-api", "SutraApiRestarting",
      "sum by (instance) (changes(sutra_process_start_time_seconds[30m])) > 2", None, "warning",
      "SutraERP API restarted more than twice in 30 minutes",
      "The API process start time changed more than twice in 30 minutes (crash loop, OOM kill or "
      "failing health checks).",
      ([s("sutra_process_start_time_seconds", API, "100 200 300 400 500 500 500 500")], "7m",
       {"instance": API["instance"]}),
      ([s("sutra_process_start_time_seconds", API, "100x10")], "7m", None))

# ---------------------------------------------------------------- PostgreSQL (postgres_exporter)
alert("sutra-postgres", "SutraPostgresDown", "pg_up == 0", "1m", "critical",
      "PostgreSQL is down",
      "postgres_exporter cannot connect to PostgreSQL for 1 minute. The API answers 503 on "
      "/health/ready and every write fails.",
      ([s("pg_up", PG, "0x10")], "5m", dict(PG)), ([s("pg_up", PG, "1x10")], "5m", None))
alert("sutra-postgres", "SutraPostgresExporterDown", 'up{job="postgres"} == 0', "5m", "warning",
      "postgres_exporter is not being scraped",
      "Prometheus cannot scrape postgres_exporter for 5 minutes; database alerts are blind.",
      ([s("up", PG, "0x10")], "8m", dict(PG)), ([s("up", PG, "1x10")], "8m", None))
alert("sutra-postgres", "SutraPostgresConnectionsHigh",
      "sum by (instance) (pg_stat_activity_count) / on (instance) "
      "max by (instance) (pg_settings_max_connections) > 0.8", "10m", "warning",
      "PostgreSQL connections above 80 % of max_connections",
      "More than 80 % of max_connections are in use for 10 minutes; new connections will soon be "
      "refused.",
      ([s("pg_stat_activity_count", {**PG, "datname": "sutra", "state": "active"}, "90x20"),
        s("pg_settings_max_connections", PG, "100x20")], "15m", {"instance": PG["instance"]}),
      ([s("pg_stat_activity_count", {**PG, "datname": "sutra", "state": "active"}, "10x20"),
        s("pg_settings_max_connections", PG, "100x20")], "15m", None))
alert("sutra-postgres", "SutraPostgresReplicationLag",
      "max by (instance) (pg_replication_lag_seconds) > 300", "5m", "warning",
      "PostgreSQL replica more than 5 minutes behind",
      "The standby replays WAL more than 5 minutes behind the primary; a failover now would lose "
      "recent writes.",
      ([s("pg_replication_lag_seconds", PG, "600x20")], "10m", {"instance": PG["instance"]}),
      ([s("pg_replication_lag_seconds", PG, "1x20")], "10m", None))
alert("sutra-postgres", "SutraPostgresLongTransaction",
      'max by (instance, datname) (pg_stat_activity_max_tx_duration{datname!~"template.*|"}) > 600',
      "5m", "warning",
      "PostgreSQL transaction open for more than 10 minutes",
      "A transaction has been open for more than 10 minutes (slow query, stuck job or idle in "
      "transaction); it holds locks and blocks vacuum.",
      ([s("pg_stat_activity_max_tx_duration", {**PG, "datname": "sutra", "state": "active"}, "900x20")],
       "10m", {"instance": PG["instance"], "datname": "sutra"}),
      ([s("pg_stat_activity_max_tx_duration", {**PG, "datname": "sutra", "state": "active"}, "2x20")],
       "10m", None))
alert("sutra-postgres", "SutraPostgresSlowQueries",
      "max by (instance, datname) (rate(pg_stat_statements_seconds_total[10m]) / "
      "clamp_min(rate(pg_stat_statements_calls_total[10m]), 1e-9)) > 1", "15m", "warning",
      "PostgreSQL statements averaging more than 1 s",
      "A statement averaged more than 1 second per call over 10 minutes (pg_stat_statements); see "
      "the PostgreSQL dashboard's slowest statements.",
      ([s("pg_stat_statements_seconds_total", {**PG, "datname": "sutra", "queryid": "42", "user": "sutra"}, "0+120x40"),
        s("pg_stat_statements_calls_total", {**PG, "datname": "sutra", "queryid": "42", "user": "sutra"}, "0+60x40")],
       "30m", {"instance": PG["instance"], "datname": "sutra"}),
      ([s("pg_stat_statements_seconds_total", {**PG, "datname": "sutra", "queryid": "42", "user": "sutra"}, "0+1x40"),
        s("pg_stat_statements_calls_total", {**PG, "datname": "sutra", "queryid": "42", "user": "sutra"}, "0+600x40")],
       "30m", None))
TBL = {**PG, "datname": "sutra", "schemaname": "finance", "relname": "payment"}
alert("sutra-postgres", "SutraPostgresTableBloat",
      "(pg_stat_user_tables_n_dead_tup > 100000) and "
      "(pg_stat_user_tables_n_dead_tup / clamp_min(pg_stat_user_tables_n_live_tup + "
      "pg_stat_user_tables_n_dead_tup, 1) > 0.2)", "1h", "warning",
      "PostgreSQL table has more than 20 % dead rows",
      "A table has over 100,000 dead rows and more than 20 % of its rows are dead for an hour: "
      "autovacuum is not keeping up (long transactions, vacuum settings).",
      ([s("pg_stat_user_tables_n_dead_tup", TBL, "500000x90"), s("pg_stat_user_tables_n_live_tup", TBL, "1000000x90")],
       "80m", dict(TBL)),
      ([s("pg_stat_user_tables_n_dead_tup", TBL, "500000x90"), s("pg_stat_user_tables_n_live_tup", TBL, "9000000x90")],
       "80m", None))
alert("sutra-postgres", "SutraPostgresDeadlocks", "sum by (instance) (increase(pg_stat_database_deadlocks[15m])) > 5",
      None, "warning", "PostgreSQL deadlocks",
      "More than 5 deadlocks in 15 minutes; requests are being aborted and retried.",
      ([s("pg_stat_database_deadlocks", {**PG, "datname": "sutra"}, "0+1x30")], "20m", {"instance": PG["instance"]}),
      ([s("pg_stat_database_deadlocks", {**PG, "datname": "sutra"}, "0x30")], "20m", None))

# ---------------------------------------------------------------- backups + WAL archive
BK = {**NODE, "kind": "logical"}
alert("sutra-backups", "SutraBackupFailed", "sutra_backup_last_run_success == 0", None, "critical",
      "The last SutraERP backup run failed",
      "The most recent backup run (sutra-backup-metrics.sh) exited non-zero. The previous good "
      "backup is still the restore point; RPO is at risk.",
      ([s("sutra_backup_last_run_success", BK, "0x10")], "2m", dict(BK)),
      ([s("sutra_backup_last_run_success", BK, "1x10")], "2m", None))
alert("sutra-backups", "SutraBackupStale",
      "time() - max by (kind) (sutra_backup_last_success_timestamp_seconds) > 26 * 3600", None, "critical",
      "No successful SutraERP backup for more than 26 hours",
      "The newest successful backup is older than 26 hours (daily schedule + 2 h grace): the "
      "24 h RPO is breached. The backup timer stopped or every run fails.",
      ([s("sutra_backup_last_success_timestamp_seconds", BK, "0x30")], "27h", {"kind": "logical"}, "1h"),
      ([s("sutra_backup_last_success_timestamp_seconds", BK, "0x30")], "25h", None, "1h"))
alert("sutra-backups", "SutraBackupMissing", "absent(sutra_backup_last_success_timestamp_seconds)", "2h", "warning",
      "No SutraERP backup has been recorded",
      "Prometheus has no backup metrics for 2 hours: the backup has never run with "
      "sutra-backup-metrics.sh, or node_exporter's textfile directory is not mounted.",
      ([s("up", NODE, "1x200")], "3h", {}),
      ([s("up", NODE, "1x200"), s("sutra_backup_last_success_timestamp_seconds", BK, "0x200")], "3h", None))
alert("sutra-backups", "SutraWalArchiveFailing", "increase(pg_stat_archiver_failed_count[15m]) > 0", None, "critical",
      "PostgreSQL WAL archiving is failing",
      "archive_command failed in the last 15 minutes: point-in-time recovery has a gap and pg_wal "
      "grows until the disk fills.",
      ([s("pg_stat_archiver_failed_count", PG, "0 0 0 1 2 3 3 3 3 3")], "6m", dict(PG)),
      ([s("pg_stat_archiver_failed_count", PG, "0x10")], "6m", None))
alert("sutra-backups", "SutraWalArchiveStale", "pg_stat_archiver_last_archive_age > 7200", "15m", "warning",
      "No WAL segment archived for 2 hours",
      "No WAL segment has been archived for 2 hours although archive_timeout forces one at least "
      "every 15 minutes: archiving is stuck.",
      ([s("pg_stat_archiver_last_archive_age", PG, "8000+60x30")], "20m", dict(PG)),
      ([s("pg_stat_archiver_last_archive_age", PG, "60x30")], "20m", None))

# ---------------------------------------------------------------- security signals
SEC = "sutra_security_events_total"


def sec(name, event, window, threshold, severity, for_, summary, description, fire_rate):
    expr = f'sum(increase({SEC}{{event="{event}"}}[{window}])) > {threshold}'
    alert("sutra-security", name, expr, for_, severity, summary, description,
          ([s(SEC, {**API, "event": event}, f"0+{fire_rate}x40")], "20m", {}),
          ([s(SEC, {**API, "event": event}, "0x40")], "20m", None))


sec("SutraLoginFailureSpike", "login_failed", "10m", 50, "warning", None,
    "More than 50 failed sign-ins in 10 minutes",
    "Failed sign-ins spiked (password spraying, a broken integration, or many users locked out). "
    "Check the auth logs by login and source address.", 10)
sec("SutraLoginThrottleSpike", "login_throttled", "10m", 10, "warning", None,
    "More than 10 throttled sign-ins in 10 minutes",
    "Sign-ins are being refused by the lockout throttle: brute force against specific accounts.", 5)
sec("SutraAccountLockouts", "account_locked", "15m", 5, "warning", None,
    "More than 5 accounts locked in 15 minutes",
    "Several logins were locked after repeated failures in a short time.", 2)
sec("SutraMfaFailureSpike", "mfa_failed", "10m", 20, "warning", None,
    "More than 20 failed second-factor attempts in 10 minutes",
    "Second-factor codes / passkeys are being rejected in bulk: guessing against stolen passwords "
    "or a clock problem on the server (TOTP).", 5)
sec("SutraPrivilegeChanges", "privilege_changed", "1h", 10, "warning", None,
    "More than 10 privilege changes in an hour",
    "Roles were granted / revoked more than 10 times in an hour. Verify against approved "
    "requests in the audit trail (platform.identity.*).", 2)
sec("SutraCrossTenantDenied", "cross_tenant_denied", "10m", 0, "warning", None,
    "Requests tried to reach another tenant",
    "A request named a tenant or institution outside its session scope and was refused (403). "
    "Normal clients never do this: treat as probing until explained.", 1)
sec("SutraWebhookSignatureFailures", "webhook_signature_failed", "10m", 2, "warning", None,
    "Payment webhooks with invalid signatures",
    "At least 3 payment-gateway webhooks in 10 minutes had a missing or wrong signature: forged "
    "callbacks, or SUTRA_PAYMENT_WEBHOOK_SECRET differs from the gateway dashboard.", 1)
sec("SutraCheckoutSignatureFailures", "checkout_signature_failed", "30m", 4, "warning", None,
    "Checkout callbacks with invalid signatures",
    "At least 5 checkout callbacks in 30 minutes failed signature verification (tampering or a "
    "wrong key secret).", 1)

# ---------------------------------------------------------------- queues / outbox / jobs
alert("sutra-queues", "SutraOutboxPublishFailing", "sum(increase(sutra_outbox_failed_total[15m])) > 10", "15m",
      "warning", "Outbox events keep failing to publish",
      "More than 10 outbox publish attempts failed in 15 minutes (NATS down or rejecting).",
      ([s("sutra_outbox_failed_total", API, "0+2x60")], "40m", {}),
      ([s("sutra_outbox_failed_total", API, "0x60")], "40m", None))
alert("sutra-queues", "SutraNatsDown", 'up{job="nats"} == 0', "2m", "critical",
      "NATS (JetStream) is not being scraped",
      "The NATS exporter cannot be scraped or NATS is down for 2 minutes: events queue up in the "
      "outbox.",
      ([s("up", {"job": "nats", "instance": "nats-exporter:7777"}, "0x10")], "5m",
       {"job": "nats", "instance": "nats-exporter:7777"}),
      ([s("up", {"job": "nats", "instance": "nats-exporter:7777"}, "1x10")], "5m", None))
alert("sutra-queues", "SutraJetStreamConsumerLag", "max(jetstream_consumer_num_pending) > 1000", "15m", "warning",
      "A JetStream consumer is more than 1000 messages behind",
      "A JetStream consumer has more than 1000 pending messages for 15 minutes; a consumer is down "
      "or too slow.",
      ([s("jetstream_consumer_num_pending", {"job": "nats", "stream_name": "SUTRA", "consumer_name": "c"}, "5000x30")],
       "20m", {}),
      ([s("jetstream_consumer_num_pending", {"job": "nats", "stream_name": "SUTRA", "consumer_name": "c"}, "3x30")],
       "20m", None))
alert("sutra-queues", "SutraIdpSyncFailing", 'max(sutra_idp_sync_links{status="failed"}) > 0', "30m", "warning",
      "Identity provider sync keeps failing",
      "Identity links have stayed in sync_status=failed for 30 minutes: Keycloak is unreachable or "
      "refuses the admin client; the idp-sync job retries with back-off.",
      ([s("sutra_idp_sync_links", {**API, "status": "failed"}, "4x40")], "35m", {}),
      ([s("sutra_idp_sync_links", {**API, "status": "failed"}, "0x40")], "35m", None))
alert("sutra-queues", "SutraIdpSyncConflict", 'max(sutra_idp_sync_links{status="conflict"}) > 0', "1h", "warning",
      "Identity links in conflict need an administrator",
      "An identity link is in sync_status=conflict (an IdP account owned elsewhere): it is not "
      "retried automatically.",
      ([s("sutra_idp_sync_links", {**API, "status": "conflict"}, "1x80")], "70m", {}),
      ([s("sutra_idp_sync_links", {**API, "status": "conflict"}, "0x80")], "70m", None))
alert("sutra-queues", "SutraIdpSyncBacklog", 'max(sutra_idp_sync_links{status="pending"}) > 100', "30m", "warning",
      "More than 100 identity links waiting to sync",
      "The idp-sync job is not draining its queue (job disabled, slow IdP, or a bulk import).",
      ([s("sutra_idp_sync_links", {**API, "status": "pending"}, "500x40")], "35m", {}),
      ([s("sutra_idp_sync_links", {**API, "status": "pending"}, "3x40")], "35m", None))
alert("sutra-queues", "SutraPaymentRecoveryStale", "max(sutra_gateway_payments_open_stale) > 0", "1h", "warning",
      "Gateway payments still open after an hour",
      "Gateway payments stayed initiated / pending for over an hour and the recovery job has not "
      "resolved them (gateway API errors, or the job is off).",
      ([s("sutra_gateway_payments_open_stale", API, "3x80")], "70m", {}),
      ([s("sutra_gateway_payments_open_stale", API, "0x80")], "70m", None))
for job in ("idp-sync", "payment_gateway_recovery"):
    nm = "SutraIdpSyncJobErrors" if job == "idp-sync" else "SutraPaymentRecoveryJobErrors"
    alert("sutra-queues", nm, f'sum(increase(sutra_job_runs_total{{job="{job}",outcome="error"}}[30m])) >= 2',
          None, "warning", f"The {job} job run failed repeatedly",
          f"The {job} scheduled job failed at least twice in 30 minutes (each run retries the "
          "backlog; persistent failure means its dependency is down).",
          ([s("sutra_job_runs_total", {**API, "job": job, "outcome": "error"}, "0+1x40")], "20m", {}),
          ([s("sutra_job_runs_total", {**API, "job": job, "outcome": "error"}, "0x40")], "20m", None))

# ---------------------------------------------------------------- integrations
CALLS = "sutra_integration_calls_total"
for name, integ, what in [("SutraRazorpayErrors", "razorpay", "Razorpay API"),
                          ("SutraS3Errors", "s3", "Object storage (S3 / MinIO)"),
                          ("SutraKeycloakAdminErrors", "keycloak_admin", "Keycloak admin API"),
                          ("SutraSmtpErrors", "smtp", "SMTP relay")]:
    bad = f'sum(increase({CALLS}{{integration="{integ}",outcome=~"error|timeout"}}[10m]))'
    allc = f'sum(increase({CALLS}{{integration="{integ}"}}[10m]))'
    alert("sutra-integrations", name, f"{bad} >= 5 and {bad} / clamp_min({allc}, 1) > 0.25", "5m", "warning",
          f"{what} calls failing", f"More than 25 % of {what} calls (at least 5) failed or timed out "
          "over 10 minutes.",
          ([s(CALLS, {**API, "integration": integ, "outcome": "error"}, "0+3x40"),
            s(CALLS, {**API, "integration": integ, "outcome": "ok"}, "0+3x40")], "25m", {}),
          ([s(CALLS, {**API, "integration": integ, "outcome": "error"}, "0+0x40"),
            s(CALLS, {**API, "integration": integ, "outcome": "ok"}, "0+30x40")], "25m", None))

# ---------------------------------------------------------------- host (node_exporter)
DISK = {**NODE, "device": "/dev/sda1", "mountpoint": "/", "fstype": "ext4"}
alert("sutra-host", "SutraHostDown", 'up{job="node"} == 0', "2m", "critical",
      "Host metrics unavailable (node_exporter down)",
      "node_exporter cannot be scraped for 2 minutes: the host is down or the exporter stopped.",
      ([s("up", NODE, "0x10")], "5m", dict(NODE)), ([s("up", NODE, "1x10")], "5m", None))
alert("sutra-host", "SutraHostDiskSpaceLow",
      f"node_filesystem_avail_bytes{{{FS}}} / node_filesystem_size_bytes{{{FS}}} < 0.15", "10m", "warning",
      "Less than 15 % disk space left",
      "A filesystem has less than 15 % free space (PostgreSQL data, WAL archive or backups).",
      ([s("node_filesystem_avail_bytes", DISK, "10x20"), s("node_filesystem_size_bytes", DISK, "100x20")],
       "15m", dict(DISK)),
      ([s("node_filesystem_avail_bytes", DISK, "50x20"), s("node_filesystem_size_bytes", DISK, "100x20")],
       "15m", None))
alert("sutra-host", "SutraHostDiskSpaceCritical",
      f"node_filesystem_avail_bytes{{{FS}}} / node_filesystem_size_bytes{{{FS}}} < 0.05", "5m", "critical",
      "Less than 5 % disk space left",
      "A filesystem is almost full; PostgreSQL stops accepting writes when its disk is full.",
      ([s("node_filesystem_avail_bytes", DISK, "3x20"), s("node_filesystem_size_bytes", DISK, "100x20")],
       "10m", dict(DISK)),
      ([s("node_filesystem_avail_bytes", DISK, "10x20"), s("node_filesystem_size_bytes", DISK, "100x20")],
       "10m", None))
alert("sutra-host", "SutraHostDiskFillingUp",
      f"predict_linear(node_filesystem_avail_bytes{{{FS}}}[6h], 24 * 3600) < 0", "1h", "warning",
      "A filesystem will be full within 24 hours",
      "At the growth rate of the last 6 hours a filesystem runs out of space within a day.",
      ([s("node_filesystem_avail_bytes", DISK, "100000-100x200")], "190m", dict(DISK)),
      ([s("node_filesystem_avail_bytes", DISK, "100000x200")], "190m", None))
alert("sutra-host", "SutraHostCpuHigh",
      '1 - avg by (instance) (rate(node_cpu_seconds_total{mode="idle"}[5m])) > 0.9', "15m", "warning",
      "Host CPU above 90 %", "The host's CPUs have been more than 90 % busy for 15 minutes.",
      ([s("node_cpu_seconds_total", {**NODE, "cpu": "0", "mode": "idle"}, "0+3x40")], "30m",
       {"instance": NODE["instance"]}),
      ([s("node_cpu_seconds_total", {**NODE, "cpu": "0", "mode": "idle"}, "0+50x40")], "30m", None))
alert("sutra-host", "SutraHostMemoryLow",
      "node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes < 0.1", "10m", "warning",
      "Less than 10 % memory available",
      "Available memory stayed below 10 % for 10 minutes; the kernel may OOM-kill PostgreSQL or the "
      "API.",
      ([s("node_memory_MemAvailable_bytes", NODE, "5x20"), s("node_memory_MemTotal_bytes", NODE, "100x20")],
       "15m", dict(NODE)),
      ([s("node_memory_MemAvailable_bytes", NODE, "50x20"), s("node_memory_MemTotal_bytes", NODE, "100x20")],
       "15m", None))

# ---------------------------------------------------------------- the monitoring itself
PROM = {"job": "prometheus", "instance": "localhost:9090"}
alert("sutra-meta", "SutraWatchdog", "vector(1)", None, "none",
      "Alerting pipeline heartbeat (always firing)",
      "Always firing. Route it to a dead-man's switch: if it stops arriving, Prometheus or "
      "Alertmanager is down.",
      ([s("up", PROM, "1x5")], "2m", {}), None)
alert("sutra-meta", "SutraRuleEvaluationFailures", "increase(prometheus_rule_evaluation_failures_total[10m]) > 0",
      None, "warning", "Prometheus rule evaluations are failing",
      "A rule group fails to evaluate; some alerts cannot fire.",
      ([s("prometheus_rule_evaluation_failures_total", {**PROM, "rule_group": "g"}, "0+1x20")], "15m",
       {**PROM, "rule_group": "g"}),
      ([s("prometheus_rule_evaluation_failures_total", {**PROM, "rule_group": "g"}, "0x20")], "15m", None))
alert("sutra-meta", "SutraAlertNotificationsFailing",
      "sum by (integration) (increase(alertmanager_notifications_failed_total[15m])) > 0", None, "warning",
      "Alertmanager cannot deliver notifications",
      "Alertmanager failed to deliver notifications (SMTP relay, chat or SMS webhook unreachable "
      "or refusing); alerts are not reaching people.",
      ([s("alertmanager_notifications_failed_total", {"job": "alertmanager", "integration": "email"}, "0+1x20")],
       "15m", {"integration": "email"}),
      ([s("alertmanager_notifications_failed_total", {"job": "alertmanager", "integration": "email"}, "0x20")],
       "15m", None))
alert("sutra-meta", "SutraScrapeTargetDown", 'up{job=~"alertmanager|grafana"} == 0', "5m", "warning",
      "A monitoring component is down", "Alertmanager or Grafana has not been scrapeable for 5 minutes.",
      ([s("up", {"job": "alertmanager", "instance": "alertmanager:9093"}, "0x10")], "8m",
       {"job": "alertmanager", "instance": "alertmanager:9093"}),
      ([s("up", {"job": "alertmanager", "instance": "alertmanager:9093"}, "1x10")], "8m", None))

# ================================================================ write rules + tests
groups, order = {}, []
for g, r, _, _ in ALERTS:
    if g not in groups:
        groups[g] = []
        order.append(g)
    groups[g].append(r)
names = [r["alert"] for _, r, _, _ in ALERTS]
assert len(names) == len(set(names)), "duplicate alert names"
os.makedirs("prometheus/rules/tests", exist_ok=True)
hdr = ("# SutraERP on-prem alert rules (Part 2 S5-monitor). GENERATED by deploy/monitoring/gen.py — edit\n"
       "# there and run `python3 gen.py`. Loaded together with deploy/observability/alerts/sutra.rules.yaml.\n"
       "# Runbooks: docs/readiness/MONITORING_AND_SUPPORT.md. promtool tests: rules/tests/.\n")
with open("prometheus/rules/sutra-onprem.rules.yaml", "w") as f:
    f.write(hdr + yaml.safe_dump({"groups": [{"name": g, "interval": "30s", "rules": groups[g]} for g in order]},
                                 sort_keys=False, width=200))

tests = []
for _, r, fire, quiet in ALERTS:
    for kind, spec in (("fires", fire), ("stays quiet", quiet)):
        if spec is None:
            continue
        series, at, labels = spec[0], spec[1], spec[2]
        interval = spec[3] if len(spec) > 3 else "1m"
        exp = [] if labels is None else [{"exp_labels": {**labels, "severity": r["labels"]["severity"]},
                                          "exp_annotations": r["annotations"]}]
        tests.append({"interval": interval, "name": f'{r["alert"]} {kind}', "input_series": series,
                      "alert_rule_test": [{"eval_time": at, "alertname": r["alert"], "exp_alerts": exp}]})
with open("prometheus/rules/tests/sutra-onprem.rules.test.yaml", "w") as f:
    f.write("# promtool test rules sutra-onprem.rules.test.yaml (GENERATED by deploy/monitoring/gen.py)\n"
            + yaml.safe_dump({"rule_files": ["../sutra-onprem.rules.yaml"], "evaluation_interval": "30s",
                              "tests": tests}, sort_keys=False, width=200))

# alert table for the readiness doc (kept in sync by CI through gen.py)
with open("ALERTS.md", "w") as f:
    f.write("<!-- GENERATED by deploy/monitoring/gen.py -->\n"
            "| Alert | Area | Severity | For | Condition |\n|---|---|---|---|---|\n")
    for g, r, _, _ in ALERTS:
        expr = r["expr"].replace("|", "\\|")
        f.write(f'| [{r["alert"]}](#{r["alert"].lower()}) | {g} | {r["labels"]["severity"]} | '
                f'{r.get("for", "—")} | `{expr}` |\n')

# ================================================================ dashboards
DSP = {"type": "prometheus", "uid": "${DS_PROMETHEUS}"}


def templating():
    return {"list": [{"name": "DS_PROMETHEUS", "label": "Prometheus", "type": "datasource",
                      "query": "prometheus", "current": {}, "hide": 0}]}


def panel(pid, title, kind, targets, x, y, w=12, h=8, unit=None, desc=None):
    p = {"id": pid, "title": title, "type": kind, "datasource": DSP, "gridPos": {"x": x, "y": y, "w": w, "h": h},
         "targets": [{"refId": chr(65 + i), "datasource": DSP, "expr": e, "legendFormat": lg}
                     for i, (e, lg) in enumerate(targets)]}
    if unit:
        p["fieldConfig"] = {"defaults": {"unit": unit}, "overrides": []}
    if desc:
        p["description"] = desc
    return p


def dash(uid, title, desc, specs):
    panels = [panel(i + 1, *sp[:5], **(sp[5] if len(sp) > 5 else {})) for i, sp in enumerate(specs)]
    return {"uid": uid, "title": title, "description": desc, "tags": ["sutraerp", "onprem"], "timezone": "browser",
            "schemaVersion": 39, "version": 1, "editable": True, "refresh": "30s",
            "time": {"from": "now-6h", "to": "now"}, "templating": templating(),
            "annotations": {"list": []}, "panels": panels}


D = {}
D["sutra-postgres"] = dash("sutra-postgres", "SutraERP · PostgreSQL", "postgres_exporter: availability, "
                           "connections, WAL archive, replication, slow statements, bloat, size.", [
    ("Up", "stat", [("pg_up", "{{instance}}")], 0, 0, {"w": 4}),
    ("Connections / max", "gauge", [("sum(pg_stat_activity_count) / max(pg_settings_max_connections)", "used")], 4, 0,
     {"w": 8, "unit": "percentunit"}),
    ("Connections by state", "timeseries", [("sum by (state) (pg_stat_activity_count)", "{{state}}")], 12, 0),
    ("WAL archived / failed (15 m)", "timeseries", [("increase(pg_stat_archiver_archived_count[15m])", "archived"),
                                                     ("increase(pg_stat_archiver_failed_count[15m])", "failed")], 0, 8),
    ("Last WAL archive age", "stat", [("pg_stat_archiver_last_archive_age", "age")], 12, 8, {"w": 6, "unit": "s"}),
    ("Replication lag", "stat", [("max(pg_replication_lag_seconds)", "lag")], 18, 8, {"w": 6, "unit": "s"}),
    ("Longest open transaction", "timeseries", [("max by (datname) (pg_stat_activity_max_tx_duration)", "{{datname}}")],
     0, 16, {"unit": "s"}),
    ("Slowest statements (mean, 10 m)", "table",
     [("topk(10, rate(pg_stat_statements_seconds_total[10m]) / clamp_min(rate(pg_stat_statements_calls_total[10m]), 1e-9))",
       "{{datname}} {{queryid}}")], 12, 16, {"unit": "s"}),
    ("Dead-row ratio (top 10 tables)", "table",
     [("topk(10, pg_stat_user_tables_n_dead_tup / clamp_min(pg_stat_user_tables_n_live_tup + pg_stat_user_tables_n_dead_tup, 1))",
       "{{schemaname}}.{{relname}}")], 0, 24, {"unit": "percentunit"}),
    ("Database size", "timeseries", [("pg_database_size_bytes", "{{datname}}")], 12, 24, {"unit": "bytes"}),
    ("Deadlocks / commits / rollbacks (5 m)", "timeseries",
     [("sum(increase(pg_stat_database_deadlocks[5m]))", "deadlocks"),
      ("sum(increase(pg_stat_database_xact_rollback[5m]))", "rollbacks")], 0, 32, {"w": 24}),
])
D["sutra-security"] = dash("sutra-security", "SutraERP · Security signals", "Failed / throttled sign-ins, MFA "
                           "failures, privilege changes, cross-tenant denials, signature failures.", [
    ("Security events (per 10 m)", "timeseries", [(f"sum by (event) (increase({SEC}[10m]))", "{{event}}")], 0, 0,
     {"w": 24}),
    ("Failed sign-ins (1 h)", "stat", [(f'sum(increase({SEC}{{event="login_failed"}}[1h]))', "failed")], 0, 8, {"w": 6}),
    ("Lockouts (1 h)", "stat", [(f'sum(increase({SEC}{{event=~"account_locked|login_throttled"}}[1h]))', "locked")],
     6, 8, {"w": 6}),
    ("MFA failures (1 h)", "stat", [(f'sum(increase({SEC}{{event=~"mfa_failed|mfa_locked"}}[1h]))', "mfa")], 12, 8,
     {"w": 6}),
    ("Privilege changes (24 h)", "stat", [(f'sum(increase({SEC}{{event="privilege_changed"}}[24h]))', "changes")], 18, 8,
     {"w": 6}),
    ("Cross-tenant denials (24 h)", "stat", [(f'sum(increase({SEC}{{event="cross_tenant_denied"}}[24h]))', "denied")],
     0, 16, {"w": 8}),
    ("Webhook signature failures (24 h)", "stat",
     [(f'sum(increase({SEC}{{event="webhook_signature_failed"}}[24h]))', "webhook")], 8, 16, {"w": 8}),
    ("Checkout signature failures (24 h)", "stat",
     [(f'sum(increase({SEC}{{event="checkout_signature_failed"}}[24h]))', "checkout")], 16, 16, {"w": 8}),
    ("401 / 403 / 429 responses", "timeseries",
     [('sum by (route) (rate(sutra_http_requests_total{status="4xx"}[5m]))', "{{route}}")], 0, 24, {"w": 24}),
])
D["sutra-queues-integrations"] = dash(
    "sutra-queues-integrations", "SutraERP · Queues, jobs & integrations",
    "Outbox → NATS JetStream, consumer lag, identity sync and payment recovery backlogs, external calls.", [
        ("Outbox pending", "timeseries", [("max(sutra_outbox_pending)", "pending")], 0, 0),
        ("JetStream consumer pending", "timeseries",
         [("max by (stream_name, consumer_name) (jetstream_consumer_num_pending)", "{{stream_name}}/{{consumer_name}}")],
         12, 0),
        ("Identity sync links not in sync", "timeseries", [("max by (status) (sutra_idp_sync_links)", "{{status}}")], 0, 8),
        ("Gateway payments open > 1 h", "stat", [("max(sutra_gateway_payments_open_stale)", "open")], 12, 8, {"w": 6}),
        ("Job errors (30 m)", "table",
         [('sum by (job) (increase(sutra_job_runs_total{outcome="error"}[30m]))', "{{job}}")], 18, 8, {"w": 6}),
        ("External calls by outcome (per 5 m)", "timeseries",
         [(f"sum by (integration, outcome) (increase({CALLS}[5m]))", "{{integration}} {{outcome}}")], 0, 16, {"w": 24}),
        ("External error ratio (10 m)", "timeseries",
         [(f'sum by (integration) (increase({CALLS}{{outcome=~"error|timeout"}}[10m])) / '
           f"clamp_min(sum by (integration) (increase({CALLS}[10m])), 1)", "{{integration}}")], 0, 24,
         {"w": 24, "unit": "percentunit"}),
    ])
D["sutra-host-backups"] = dash("sutra-host-backups", "SutraERP · Host & backups",
                               "node_exporter CPU / memory / disk and the backup / WAL-archive status.", [
    ("CPU busy", "timeseries", [('1 - avg by (instance) (rate(node_cpu_seconds_total{mode="idle"}[5m]))', "{{instance}}")],
     0, 0, {"unit": "percentunit"}),
    ("Memory available", "timeseries",
     [("node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes", "{{instance}}")], 12, 0, {"unit": "percentunit"}),
    ("Disk free", "bargauge", [(f"node_filesystem_avail_bytes{{{FS}}} / node_filesystem_size_bytes{{{FS}}}", "{{mountpoint}}")],
     0, 8, {"unit": "percentunit"}),
    ("Disk I/O busy", "timeseries", [("rate(node_disk_io_time_seconds_total[5m])", "{{device}}")], 12, 8,
     {"unit": "percentunit"}),
    ("Hours since last successful backup", "stat",
     [("(time() - max by (kind) (sutra_backup_last_success_timestamp_seconds)) / 3600", "{{kind}}")], 0, 16, {"w": 8}),
    ("Last backup run succeeded", "stat", [("sutra_backup_last_run_success", "{{kind}}")], 8, 16, {"w": 8}),
    ("Last backup duration / size", "stat",
     [("sutra_backup_last_duration_seconds", "seconds"), ("sutra_backup_last_size_bytes", "bytes")], 16, 16, {"w": 8}),
    ("WAL archive failures (24 h)", "stat", [("increase(pg_stat_archiver_failed_count[24h])", "failed")], 0, 24, {"w": 12}),
    ("Load average", "timeseries", [("node_load5", "{{instance}}")], 12, 24),
])
os.makedirs("grafana/dashboards", exist_ok=True)
for uid, d in D.items():
    with open(f"grafana/dashboards/{uid}.json", "w") as f:
        f.write(json.dumps(d, indent=2, ensure_ascii=False) + "\n")
print("ok:", len(ALERTS), "alerts in", len(order), "groups;", len(tests), "tests;", "dashboards", list(D))
