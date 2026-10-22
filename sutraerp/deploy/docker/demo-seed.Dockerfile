# One-shot demo seed for the Batch 3 + 4 click-through (README "Batch 3 + 4 Docker click-through").
#   docker compose --profile demo run --rm --build demo-seed
# Node (API calls through the e2e harness) + psql (foundation-table rows), nothing else: no pnpm
# install, no repo mount. Talks to the compose services `api` and `postgres`.
FROM node:22-alpine
RUN apk add --no-cache postgresql-client
WORKDIR /seed
COPY apps/web/e2e/lib/harness.mjs apps/web/e2e/lib/harness.mjs
# A1: harness.mjs imports the per-run dev password helper (SUTRA_DEV_AUTH_PASSWORD).
COPY apps/web/e2e/lib/dev-password.mjs apps/web/e2e/lib/dev-password.mjs
# Rich demo data (staff, roster, time, expenses, fees + GST, exams, campus, notices).
COPY apps/web/e2e/lib/demo-rich.mjs apps/web/e2e/lib/demo-rich.mjs
COPY apps/web/e2e/demo-seed.mjs apps/web/e2e/demo-seed.mjs
ENTRYPOINT ["node", "apps/web/e2e/demo-seed.mjs"]
