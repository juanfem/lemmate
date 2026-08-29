# Parser conformance corpus

Each `NAME.md` has a `NAME.json` holding the expected `NoteIndex` (see
`crates/core/src/markdown.rs`). `plain_text` is excluded from the contract.

The Rust indexer is checked by `cargo test -p notes-core corpus`. The TypeScript parser in
`ui/` must produce the same JSON for every case; add a case whenever the two disagree.

Regenerate an expectation after an intentional change, then review the diff by hand:

    cargo run -q -p notes-cli -- index corpus/NAME.md --json > corpus/NAME.json
