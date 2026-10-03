# Documentation

Start at the [repository README](../README.md) to run the app; this directory explains how it works and how to work on it.

| Document | Contents |
| --- | --- |
| [configuration.md](configuration.md) | every `.env` variable and `config.toml` key, lookup order, provider examples |
| [servers.md](servers.md) | the three HTTP endpoints the app calls and how to smoke-test them |
| [troubleshooting.md](troubleshooting.md) | permissions, offline status, startup errors, log location |
| [architecture.md](architecture.md) | what the app is, crate responsibilities, data flow, boundaries |
| [dependencies.md](dependencies.md) | allowed crate dependency edges and the pure-crate rule |
| [contracts.md](contracts.md) | invariants between modules and the tests that verify them |
| [system-audio.md](system-audio.md) | how the Them stream captures system audio, backends and caveats |
| [decisions.md](decisions.md) | design records: question, chosen option with rationale, rejected options |
| [development.md](development.md) | build, run, validation commands, tests, fixtures, scripts |
| [manual-checks.md](manual-checks.md) | the pre-release checklist that needs a real desktop session |
