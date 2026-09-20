# Fuzz Targets

Cargo-fuzz targets are intentionally outside the main workspace so normal builds do
not require the nightly libFuzzer toolchain.

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run query_parser -- -max_total_time=60
```

The nightly CI workflow runs a bounded query-parser smoke target. New parser,
canonicalization, path-normalization, recovery-plan, or blob-header code should add a
focused target before release.
