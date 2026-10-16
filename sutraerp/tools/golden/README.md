# Golden vectors from Everything

SutraERP ports pure business logic from [HeyPuter/everything](https://github.com/HeyPuter/everything)
(MIT) into Rust. To prove a port behaves like the original, each spec in `specs/` runs the
**original JS function**, pinned to the commit in `pin.json`, over deterministic inputs, and writes a
JSON fixture next to the Rust code. The Rust tests replay every case.

```bash
node tools/golden/generate.mjs          # (re)write fixtures; clones the pinned files into .cache/
node tools/golden/generate.mjs --check  # CI: fail if a committed fixture is stale
```

Adding a port: write `specs/<name>.mjs` (`fn`, `module`, `paths`, `out`, `cases()`, `run()`),
generate, commit the fixture, and add a Rust test that `include_str!`s it. Deliberate deviations
from the original (Indian formats, exact Decimal maths) belong in the spec's inputs or a documented
mapping in the Rust test, never in a hand-edited fixture.

| Spec | Original | Rust port | Fixture |
|---|---|---|---|
| money-allocate | `src/accounting/src/lib/money.js` `allocate` | `sutra_kernel::money::allocate` | `crates/kernel/tests/golden/money_allocate.json` |
