# Audit of 2026-10-03

Baseline audit of the desktop against OpenAlgo web, taken before the port began.
File and line references point at the code as it stood at commit 6947b4c.
These are working documents for the port, not user documentation.

| Report | Covers |
| --- | --- |
| 02-auth-flow.md | Setup, login, broker login, OAuth callbacks, session persistence |
| 03-brokers.md | Existing adapters versus web, specs for every web broker, family design |
| 04-api-parity.md | Every /api/v1 route and the WebSocket protocol, desktop drift, web test inventory |
| 05-frontend.md | Page and route parity, broken wiring, /trading port plan, frontend tests |
| 06-build-ci.md | Build, lint and test baseline, CI design, testing strategy |
| 07-sandbox.md | Web sandbox engine, desktop gaps, Rust engine design and test plan |

The security report is held back until its findings are fixed.
