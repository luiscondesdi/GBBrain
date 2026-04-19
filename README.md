# GBBrain

GBBrain is a headless GB/GBC/GBA emulator built for AI agents debugging Game Boy projects.

The project target is not a human-facing desktop emulator. It is a deterministic emulation runtime with:

- machine-readable state inspection
- reproducible stepping and tracing
- scriptable ROM loading and execution control
- debugging hooks that agents can query without a GUI

## Initial Direction

- Language: Rust
- Shape: cargo workspace
- Scope: start with DMG/GB core, then extend to CGB, then GBA
- Primary interface: library-first, CLI second

## Near-Term Goals

1. Load a ROM and run it under deterministic control.
2. Expose full ROM, RAM, and CPU register inspection APIs.
3. Support stepping, breakpoints, and structured execution events.
4. Render a frame for a precise emulated point in time without a GUI.
5. Add ROM-based validation and conformance testing before expanding hardware coverage.

## Starter Feature Contract

The first usable version for an AI agent should support:

- load a ROM and optional BIOS
- run continuously or until a breakpoint/event
- step by instruction
- inspect ROM bytes, RAM bytes, and CPU registers at any stop point
- set and clear execution and memory breakpoints
- capture a frame buffer for a specific emulated moment

Those capabilities are the baseline product surface, not optional tooling.

See [docs/implementation-plan.md](docs/implementation-plan.md) for the phased plan.
