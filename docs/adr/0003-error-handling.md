# `thiserror` in the library, `anyhow` only in binaries

`keelrs` returns one typed `Error` enum, derived with `thiserror`, so callers can match on what went wrong and the crate's public API exposes no foreign error type. `anyhow` is for binaries only (the examples' `main`), where errors are reported, not matched; it never appears in `keelrs`'s dependencies or signatures.
