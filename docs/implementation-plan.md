# GBBrain Implementation Plan

## Product Goal

Build a deterministic, headless emulator for GB, GBC, and GBA that AI agents can drive programmatically to debug game projects, validate ROM behavior, inspect hardware state, and reproduce failures.

## Non-Goals

- No desktop GUI
- No real-time audio/video UX for humans
- No shader/filter pipeline
- No broad plugin system in v1

## Core Principles

1. Deterministic execution
2. Scriptable control surface
3. Introspectable machine state
4. Reproducible save/load snapshots
5. Hardware accuracy prioritized over wall-clock speed in debug mode
6. Frame rendering available as data, not as a GUI surface

## Architecture

### Workspace Layout

- `crates/core`: shared traits, clocking, memory abstractions, save-state format
- `crates/gb`: DMG/CGB implementation
- `crates/gba`: GBA implementation
- `crates/cli`: headless executable for batch runs, stepping, traces, snapshots
- `roms/`: local developer ROM fixtures, ignored by git by default
- `docs/`: architecture, roadmap, agent protocol notes

### Runtime Model

- Library-first architecture with a thin CLI wrapper
- Separate machine implementations per platform with shared debugging traits
- Two execution modes:
  - `accurate`: prioritize correctness and determinism
  - `fast`: relaxed instrumentation for throughput

### Agent-Facing Surface

The first-class API should support:

- load ROM and optional BIOS
- reset cold/warm
- step instruction or step frame
- run until breakpoint/watchpoint/event
- inspect complete ROM data, RAM ranges, CPU registers, and PPU/APU/timer state
- capture traces and structured execution events
- save/load full machine snapshots
- render the current frame buffer for an exact emulated state

## Starter Scope

Before broader emulator accuracy work, the project should provide these agent-visible capabilities end to end:

1. Load a ROM into a machine instance.
2. Run until stop conditions or explicit user control.
3. Read ROM bytes, RAM bytes, and CPU registers at any stop point.
4. Set breakpoints and watchpoints.
5. Step instruction-by-instruction.
6. Render a frame corresponding to the current machine state.

This means early implementation effort should prioritize a coherent debug/runtime API over breadth of hardware support.

## Phased Plan

### Phase 0: Project Foundation

- Initialize cargo workspace and repository standards
- Define machine/debugger traits around load, run, inspect, breakpoint, and frame capture
- Define error model and structured event model
- Decide snapshot serialization strategy
- Add CI, formatting, linting, and test layout

### Phase 1: DMG Minimum Viable Core

- CPU register model and instruction decode/execute loop
- MMU with cartridge ROM/RAM mapping
- Timer, interrupt controller, joypad register model
- Basic PPU timing model sufficient for ROM test progression
- Serial output capture for test ROM diagnostics
- Frame buffer extraction for the current LCD state

Deliverable:

- Run CPU-focused Game Boy test ROMs
- Step instruction-by-instruction deterministically
- Query ROM/RAM/register state from code and CLI
- Stop and resume on breakpoints
- Render a headless frame buffer snapshot

### Phase 2: Debugging Interface

- Breakpoints on PC, opcode class, memory read/write, interrupts
- Ring-buffer trace stream
- Snapshot and restore
- Symbol loading hooks for RGBDS/other toolchains
- JSON output mode for agent consumption

Deliverable:

- Agents can run automated debug loops against ROMs without screen scraping

### Phase 3: PPU and CGB Expansion

- Improve PPU correctness and LCD mode transitions
- VRAM/OAM timing constraints
- Palette, banking, DMA details
- CGB double-speed and additional hardware state

Deliverable:

- Support more complete GB/CGB test coverage and graphics-sensitive debugging

### Phase 4: GBA Core Bootstrap

- ARM7TDMI execution core
- GBA memory map and wait-state handling
- Timers, interrupts, DMA, keypad
- Minimal video path for stateful correctness and frame stepping

Deliverable:

- Early GBA bring-up with headless debugging and deterministic snapshots

### Phase 5: Tooling and Agent Integration

- Stable CLI commands for batch execution
- Optional JSON-RPC or stdio control protocol
- Test-case minimization helpers
- Failure artifact bundles: snapshot + trace + ROM metadata

Deliverable:

- AI agents can reproduce, inspect, and report emulator/game failures end-to-end

## Testing Strategy

- Unit tests for CPU instructions, flags, and MMU behavior
- Golden tests for traces and save-state round trips
- Integration tests with public emulator test ROMs
- Differential testing against trusted emulators where useful
- Determinism tests: same input stream must yield identical state hash

## Immediate Backlog

1. Finalize workspace crate boundaries around runtime, GB machine, and CLI surfaces.
2. Implement shared traits in `core` for ROM loading, execution control, inspection, breakpoints, and frame capture.
3. Scaffold `gb` crate with CPU state, memory map, and stepping API.
4. Add a minimal CLI that loads a ROM path and exposes run/step/inspect commands.
5. Wire baseline test harness for ROM-driven execution and deterministic state assertions.

## Open Decisions

- Save-state encoding: bincode/postcard/custom versioned format
- CLI-only control vs stdio JSON-RPC for agent orchestration
- Accuracy target for first PPU milestone
- BIOS handling policy and test-fixture strategy
