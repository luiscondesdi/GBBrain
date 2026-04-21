# GBBrain

GBBrain is a headless GB/GBC/GBA emulator built for AI agents debugging Game Boy projects.

It is not meant to be a traditional player-facing emulator. The goal is to provide a deterministic runtime that an agent can drive programmatically, inspect deeply, and pause at exactly the point where a game or toolchain bug becomes visible.

## What It Is For

GBBrain is being built for workflows like:

- loading a ROM and running it under deterministic control
- reading ROM, RAM, VRAM, and CPU register state without scraping a GUI
- stepping instruction-by-instruction or running until a stop condition
- placing breakpoints and watchpoints on execution or memory access
- capturing a frame buffer at a precise emulated moment for analysis
- generating reproducible traces and snapshots for automated debugging loops

## Project Status

GBBrain is well past bootstrap and into real DMG hardware work. The current priority is closing the remaining accuracy gap with `mooneye-gb`'s execution model while keeping the AI-facing debugger surface strong and stable for local agent use.

For the current implementation checkpoint, suite baseline, and verified debugger coverage, see [docs/current-status.md](docs/current-status.md).

## Design Goals

- Library-first, CLI second
- Hardware accuracy before interface stability
- Deterministic execution
- Machine-readable state inspection
- Headless frame rendering as raw data
- Debugging and test automation before end-user features

If the hardware model requires changing the debug interface, snapshot schema, or CLI behavior, GBBrain should change those surfaces rather than compromise on accuracy.

## Scope

The planned platform order is:

1. DMG / original Game Boy
2. CGB / Game Boy Color
3. GBA / Game Boy Advance

The immediate focus is a strong DMG debugging core with the right APIs. Breadth comes after a reliable inspection and control model.

The current architectural direction is to move the DMG core toward an explicit machine-cycle execution model, following the same broad approach used by `mooneye-gb`: ordered read/write/tick/prefetch steps, with timer, DMA, interrupt, and PPU state advancing on each cycle.

## Starter Agent Contract

The first genuinely usable version should let an AI agent:

- load a ROM and optional BIOS
- run continuously or until a breakpoint, watchpoint, or event
- step by instruction
- inspect ROM bytes, RAM bytes, and CPU registers at any stop point
- capture a frame buffer from the current machine state
- save enough state to reproduce failures reliably

If those capabilities are missing, the emulator is not yet meeting its main purpose.

## Repository Layout

- `crates/core`: shared machine, debugging, and rendering interfaces
- `crates/gb`: Game Boy implementation scaffolding
- `crates/cli`: headless command-line entry point
- `docs/`: implementation plan and architecture notes

## Build

```bash
cargo check
```

## AI Interface

GBBrain now includes a first machine-readable control surface over stdio:

```bash
cargo run --bin gbbrain -- serve
```

The server reads one JSON command per line from stdin and writes one JSON response per line to stdout. The process keeps a persistent emulator session alive, which makes it suitable for AI-agent clients.

Protocol details and examples are in [docs/ai-interface.md](docs/ai-interface.md).

`load_rom` accepts an optional `model` field so the client can select the target system explicitly: `dmg0`, `dmg`, `mgb`, `sgb`, or `sgb2`.

## Test ROMs

GBBrain is expected to use external test ROM suites such as Blargg and Mooneye during development, but those ROM binaries should stay local and untracked. The repository ignores `roms/`, `test-roms/`, and common Game Boy ROM extensions by default.

Setup notes are in [docs/test-roms.md](docs/test-roms.md).

## References

- Heavily inspired by `mooneye-gb`: https://github.com/Gekkio/mooneye-gb
- Hardware behavior is guided primarily by Pan Docs and related GBDev documentation.

## Roadmap

The full phased plan lives in [docs/implementation-plan.md](docs/implementation-plan.md).
