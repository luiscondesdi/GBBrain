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

## Current Status

The repository is well past bootstrap and into real DMG hardware work.

Current state:

- The DMG core now has a substantial instruction subset, explicit cycle helpers, prefetch-aware execution, timer/interrupt plumbing, DMA modeling, model-specific startup profiles, and a growing execution-state machine around `Running` / `Halt` / `InterruptDispatch`.
- The AI-facing stdio interface is active development tooling, not a placeholder. It supports machine control, inspection, breakpoints/watchpoints, traces, snapshots, disassembly, direct system-address reads/writes, and explicit model selection on `load_rom`.
- The current architecture direction is still to converge further toward `mooneye-gb`'s execution model and away from opcode-local timing patches.

Latest confirmed external suite baseline from this repo state:

- Blargg: `pass=12 fail=4 unsupported_or_error=0`
- Mooneye `acceptance`: partial confirmed baseline from the latest run is at least `31 pass / 9 fail`, with the run stalling in the long tail before a full summary

Current confirmed Blargg failures:

- `instr_timing`
- `mem_timing`
- `mem_timing-2`
- `oam_bug`

Current confirmed Mooneye failures in the observed portion of the latest run:

- `bits/unused_hwio-GS`
- `boot_div-S`
- `boot_div-dmg0`
- `boot_div-dmgABCmgb`
- `boot_div2-S`
- `boot_hwio-dmg0`
- `boot_hwio-dmgABCmgb`
- `di_timing-GS`
- `halt_ime1_timing2-GS`

So the current frontier is no longer opcode coverage. It is startup-state accuracy, interrupt/HALT edge cases, memory timing, OAM behavior, and the larger PPU/LCD/STAT side.

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

Supported commands:

- `load_rom`
- `reset`
- `step`
- `run_for_cycles`
- `run_for_instructions`
- `run`
- `snapshot`
- `inspect_memory`
- `read_address`
- `write_address`
- `disassemble`
- `save_snapshot`
- `load_snapshot`
- `add_breakpoint`
- `clear_breakpoints`
- `get_trace`
- `clear_trace`
- `get_serial_output`
- `clear_serial_output`
- `render_frame`
- `shutdown`

Protocol details and examples are in [docs/ai-interface.md](docs/ai-interface.md).

`load_rom` accepts an optional `model` field so the client can select the target system explicitly: `dmg0`, `dmg`, `mgb`, `sgb`, or `sgb2`.

## Test ROMs

GBBrain is expected to use external test ROM suites such as Blargg and Mooneye during development, but those ROM binaries should stay local and untracked. The repository ignores `roms/`, `test-roms/`, and common Game Boy ROM extensions by default.

Setup notes are in [docs/test-roms.md](docs/test-roms.md).

## Roadmap

The full phased plan lives in [docs/implementation-plan.md](docs/implementation-plan.md).
