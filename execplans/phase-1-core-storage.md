# Implement Header and Mmap Core

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

This phase makes the project tangible: a byte-level message header that is provably 64 bytes and a safe, minimal memory-mapped file layer that can write and read a header plus payload. After this change, a developer can run tests that validate header size, alignment, CRC behavior, and a small append/read round trip through a memory-mapped file.

## Progress

- [x] (2026-01-15 00:00Z) ExecPlan created for Phase 1.
- [ ] (2026-01-15 00:00Z) Define `MessageHeader` layout and serialization with 64-byte enforcement.
- [ ] (2026-01-15 00:00Z) Implement mmap file creation/opening and safe slice access.
- [ ] (2026-01-15 00:00Z) Add tests for size, alignment, CRC, and round-trip append/read.

## Surprises & Discoveries

No surprises yet. Update this section if any memory-mapping quirks, alignment issues, or CRC behavior deviates from expectations.

## Decision Log

- Decision: Use `memmap2` for memory-mapped file handling and `crc32fast` for CRC32.
  Rationale: Both are small, stable crates that avoid unsafe code in most call sites and are widely used for this exact purpose.
  Date/Author: 2026-01-15, Codex
- Decision: Implement explicit header serialization to a 64-byte array instead of transmuting structs.
  Rationale: This avoids unsafe layout assumptions while still allowing alignment tests on the struct itself.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Not started. This will be updated after tests pass and the round-trip scenario is demonstrated.

## Context and Orientation

The repository currently contains a bootstrap Rust crate with placeholder modules and a single smoke test. The relevant files are `Cargo.toml`, `src/lib.rs`, and placeholder modules such as `src/header.rs` and `src/mmap.rs`. The target design is described in `docs/DESIGN.md`, including a 64-byte `MessageHeader` and an `mmap`-based append-only storage model. There is no existing storage implementation yet.

## Plan of Work

First, update `Cargo.toml` to include dependencies for memory mapping and CRC, and add a dev dependency for temporary directories used in tests. Then, implement `MessageHeader` in `src/header.rs` with `#[repr(C, align(64))]` and fields matching the design document. Add explicit `to_bytes` and `from_bytes` methods to serialize into a `[u8; 64]`, plus helpers to compute and validate CRC32 for payloads. Next, implement `src/mmap.rs` with a small `MmapFile` wrapper that can create or open a file at a fixed size, expose `as_slice` and `as_mut_slice`, and provide a checked `range_mut(offset, len)` method that returns a bounded mutable slice. Finally, add tests in `src/header.rs` and in a new integration test in `tests/` that mmaps a file, writes a header and payload, flips the valid flag, and reads it back to verify payload and CRC.

## Concrete Steps

From the repository root, edit these files:

  - `Cargo.toml`: add `memmap2 = "0.9"` and `crc32fast = "1"` under `[dependencies]`, and `tempfile = "3"` under `[dev-dependencies]`.
  - `src/header.rs`: define `MessageHeader` with the design fields and a `_pad: [u8; 36]` to ensure a 64-byte size. Implement:
      - `impl MessageHeader { pub fn new(...) -> Self }`
      - `pub fn to_bytes(&self) -> [u8; 64]`
      - `pub fn from_bytes(bytes: &[u8; 64]) -> Result<Self>`
      - `pub fn crc32(payload: &[u8]) -> u32`
  - `src/mmap.rs`: define `pub struct MmapFile { file: File, map: MmapMut, len: usize }` and functions:
      - `pub fn create(path: &Path, len: usize) -> Result<MmapFile>`
      - `pub fn open(path: &Path) -> Result<MmapFile>`
      - `pub fn as_slice(&self) -> &[u8]`
      - `pub fn as_mut_slice(&mut self) -> &mut [u8]`
      - `pub fn range_mut(&mut self, offset: usize, len: usize) -> Result<&mut [u8]>`
  - `tests/round_trip.rs`: write a test that creates a temp file, mmaps it, writes a header and payload, and reads them back to verify size/alignment and CRC.

Then run:

  (cwd: /Users/zjx/Documents/chronicle-rs)
  cargo test

Expected output should include the new tests and report all passing.

## Validation and Acceptance

Acceptance is met when:

- `cargo test` reports all tests passing.
- `MessageHeader` size is 64 bytes and alignment is 64 bytes, validated by tests using `std::mem::size_of` and `std::mem::align_of`.
- CRC32 matches for a known payload, with one test verifying a specific constant (for example, `b"hello"`).
- The round-trip test writes header and payload into a mmap slice and reads it back, confirming payload equality and CRC correctness.

A short example of a successful test run:

  running 3 tests
  test header::tests::header_size_and_alignment ... ok
  test header::tests::crc_matches_known_payload ... ok
  test round_trip::append_read_round_trip ... ok
  test result: ok. 3 passed; 0 failed

## Idempotence and Recovery

These steps are safe to rerun. If a test fails due to an incorrect file size or offset in the mmap, delete the temporary directory created by `tempfile` or rerun the test; it will recreate everything. No persistent on-disk state is required for this phase.

## Artifacts and Notes

Not started. After implementation, include the `cargo test` output snippet and any short diffs that capture header layout or mmap functions.

## Interfaces and Dependencies

Dependencies to add:

- `memmap2` for `MmapMut` and file mapping.
- `crc32fast` for CRC32 over payload bytes.
- `tempfile` as a dev dependency for test isolation.

Public interfaces to define:

In `src/header.rs`, define:

    #[repr(C, align(64))]
    pub struct MessageHeader {
        pub length: u32,
        pub seq: u64,
        pub timestamp_ns: u64,
        pub flags: u8,
        pub type_id: u16,
        pub _reserved: u8,
        pub checksum: u32,
        pub _pad: [u8; 36],
    }

    impl MessageHeader {
        pub fn new(length: u32, seq: u64, timestamp_ns: u64, type_id: u16, checksum: u32) -> Self;
        pub fn to_bytes(&self) -> [u8; 64];
        pub fn from_bytes(bytes: &[u8; 64]) -> crate::Result<Self>;
        pub fn crc32(payload: &[u8]) -> u32;
    }

In `src/mmap.rs`, define:

    pub struct MmapFile { /* file, map, len */ }

    impl MmapFile {
        pub fn create(path: &Path, len: usize) -> crate::Result<Self>;
        pub fn open(path: &Path) -> crate::Result<Self>;
        pub fn as_slice(&self) -> &[u8];
        pub fn as_mut_slice(&mut self) -> &mut [u8];
        pub fn range_mut(&mut self, offset: usize, len: usize) -> crate::Result<&mut [u8]>;
    }

Note: The struct layout test should verify `MessageHeader` size and alignment, but all I/O should use `to_bytes` and `from_bytes` to avoid unsafe casts.

Change note: Initial ExecPlan drafted on 2026-01-15 to cover Phase 1 scope.
