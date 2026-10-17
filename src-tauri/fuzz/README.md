# Fuzzing

`cargo fuzz` runs safe mode's read-only check (`src/readonly.rs`) on generated text, looking for input that makes
it panic. The property tests in `readonly.rs` and `db.rs` cover what it should answer; this covers that it always
answers.

Needs a nightly toolchain and cargo-fuzz (`cargo install --locked cargo-fuzz`). From `src-tauri`:

```sh
cargo +nightly fuzz run read_only -s none -- -max_total_time=300
```

`-s none` skips the address sanitizer: the app forbids unsafe code, so a panic is what there is to find, and the
uninstrumented build is much quicker. Findings land in `fuzz/artifacts/read_only/`; replay one with
`cargo +nightly fuzz run read_only -s none fuzz/artifacts/read_only/<file>`.
