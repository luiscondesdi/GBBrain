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
- Deterministic execution
- Machine-readable state inspection
- Headless frame rendering as raw data
- Debugging and test automation before end-user features

## Scope

The planned platform order is:

1. DMG / original Game Boy
2. CGB / Game Boy Color
3. GBA / Game Boy Advance

The immediate focus is a strong DMG debugging core with the right APIs. Breadth comes after a reliable inspection and control model.

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

This repository is in the bootstrap phase. The current code defines the initial API shape and workspace layout, but the emulator core is still a scaffold. The next implementation steps are:

1. build a real DMG CPU and memory map
2. make breakpoints and stepping functional
3. expose stable inspection APIs for ROM, RAM, and registers
4. return actual rendered frame data from the PPU state

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

Supported MVP commands:

- `load_rom`
- `reset`
- `step`
- `run`
- `snapshot`
- `inspect_memory`
- `add_breakpoint`
- `clear_breakpoints`
- `get_trace`
- `clear_trace`
- `get_serial_output`
- `clear_serial_output`
- `render_frame`
- `shutdown`

Protocol details and examples are in [docs/ai-interface.md](docs/ai-interface.md).

## Test ROMs

GBBrain is expected to use external test ROM suites such as Blargg and Mooneye during development, but those ROM binaries should stay local and untracked. The repository ignores `roms/`, `test-roms/`, and common Game Boy ROM extensions by default.

Setup notes are in [docs/test-roms.md](docs/test-roms.md).

## Roadmap

The full phased plan lives in [docs/implementation-plan.md](docs/implementation-plan.md).
