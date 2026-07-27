# Vendored noq-udp — why this exists

This is the published crates.io `noq-udp-1.1.0` tarball (sha256
`bde7a5d5102f1cff03d482240f0ed20551661f63663620f4b26112ed751165e9`), minus the
packaging metadata (`.cargo_vcs_info.json`, `Cargo.toml.orig`, `release.toml`,
`Cargo.lock`), carried via `[patch.crates-io]` for exactly one reason:

**egdod#3** — upstream decodes cmsg payloads with an aligned `ptr::read` behind
`assert!(align_of::<T>() <= align_of::<C>())` (`src/cmsg/mod.rs`). musl declares
`struct cmsghdr` with alignment 4 where glibc says 8, so the assert fails for
`SCM_TIMESTAMPNS`'s `timespec` (align 8) and the static musl agent aborts on its
first received datagram.

The entire deviation from the tarball is in `src/cmsg/mod.rs`: `decode` uses
`ptr::read_unaligned`, `Encoder::push` uses `ptr::write_unaligned`, both
alignment asserts are gone, and a regression test module pins the behavior with
a musl-shaped (align-4) cmsghdr and a deliberately misaligned payload. One
implementation for every libc — no glibc/musl cfg forks.

Delete this whole directory and the `[patch.crates-io]` entry as soon as
upstream (github.com/n0-computer/noq) ships an unaligned-read fix in a released
`noq-udp` and `cargo update -p noq-udp` pulls it.
