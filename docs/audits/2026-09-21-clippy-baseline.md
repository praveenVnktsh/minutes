# Clippy Baseline

Measured on 2026-09-21 from `foreman/minutes/PRA-490`, after staging the
required `llama-helper` sidecar:

```text
cargo clippy -p meetily -p llama-helper --all-targets 2>&1 | grep -c "^warning"
247
```

The raw count includes repeated all-target diagnostics, two Cargo manifest
warnings, and build-script status messages emitted with `cargo:warning`.
Clippy reported 185 warnings for the `meetily` library, 195 for its test target
(185 duplicates), four for `llama-helper`, and one for the build script. Three
deny-level diagnostics also prevented compilation.

## Findings By Lint

| Finding | Primary count | Main locations |
| --- | ---: | --- |
| `clippy::incompatible_msrv` | 42 | audio, summary, crate root |
| redundant formatting borrows | 31 | API providers and logging |
| `clippy::redundant_closure` | 10 | multiple modules |
| `clippy::module_inception` | 10 | provider and service modules |
| `clippy::ptr_arg` | 9 | audio and ASR paths |
| `clippy::needless_borrow` | 9 | multiple modules |
| `clippy::useless_conversion` | 8 | database and analytics |
| excessive argument count | 9 | audio and API orchestration |
| `clippy::field_reassign_with_default` | 5 | tests and configuration |
| `clippy::derivable_impls` | 4 | state/configuration types |
| complex type aliases | 3 | async/shared state |
| all other Clippy and rustc findings | 45 | repository-wide |

## Findings By Module

| Area | Baseline findings | Notable risks |
| --- | ---: | --- |
| `src/api/api.rs` | 33 | redundant borrows, argument-heavy helpers |
| `src/audio` | 51 | ignored read amounts, buffer APIs, dead diarization code |
| Whisper and Parakeet engines | 14 | ignored read amounts, buffer APIs |
| `src/summary` | 28 before MSRV update | conversions and avoidable allocations |
| database, analytics, notifications | 19 | redundant conversions and closures |
| crate root, calendar, activity, config | 31 before MSRV update | module naming and state construction |
| build script and `llama-helper` | 5 | iterator traversal and ignored I/O amount |

## Allowed Or Ignored Findings

| Lint | Scope | Reason |
| --- | --- | --- |
| None | N/A | The baseline added no new suppressions. Any necessary suppression must include a local rationale. |
