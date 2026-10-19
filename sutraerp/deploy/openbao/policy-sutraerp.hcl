# OpenBao policy `sutraerp-<env>` (9B, docs/batch9-contracts.md §5): read-only access to the
# SutraERP KV v2 groups of ONE environment. `__ENV__` is replaced by seed-dev.sh / the operator
# (`sed "s/__ENV__/production/g" policy-sutraerp.hcl | bao policy write sutraerp-production -`).
path "kv/data/sutraerp/__ENV__/*" {
  capabilities = ["read"]
}

path "kv/metadata/sutraerp/__ENV__/*" {
  capabilities = ["read", "list"]
}
