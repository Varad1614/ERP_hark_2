# OpenSearch (slice 9C)

`templates/<projection>.json`: index templates (`*-<projection>-v1`) with the exact mappings the API
creates (`sutra_infra::opensearch::mappings`; a unit test keeps both identical). The API creates
`<prefix>-<projection>-v1` with alias `<prefix>-<projection>` itself (`SUTRA_SEARCH=opensearch`),
so applying the templates is optional (`PUT _index_template/sutra-<projection>`).

Projections v1: `students`, `staff`, `library-catalogue` (contract docs/batch9-contracts.md §8).
Projector job `search_project` (outbox → `_bulk`, checkpoint `platform.search_checkpoint`) and
rebuild job `search_rebuild` (runs when a projection has no checkpoint row or
`rebuild_requested_at` is set: `UPDATE platform.search_checkpoint SET rebuild_requested_at = now()`).
