# Current Status

This document tracks the latest confirmed emulator and debugger checkpoint without turning the main project README into a changelog.

## Engine Status

- The DMG core now has a large implemented instruction subset, explicit cycle helpers, prefetch-aware execution, timer/interrupt plumbing, DMA modeling, model-specific startup profiles, and an execution-state machine around `Running` / `Halt` / `InterruptDispatch`.
- The opcode engine has been refactored away from a single inline dispatcher toward range-based decode with typed operand, control, and register-pair helpers, following `mooneye-gb`'s architectural direction more closely.
- The current architecture direction is still to converge further toward `mooneye-gb`'s execution model and away from opcode-local timing patches.

## Debugger Status

The AI-facing stdio interface is active development tooling, not a placeholder. It supports machine control, inspection, breakpoints/watchpoints, traces, snapshots, disassembly, direct system-address reads/writes, explicit model selection on `load_rom`, and has been smoke-tested end to end after the latest engine refactor.

Recently re-verified command paths:

- `help`
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

## External Test Suites

Latest confirmed external suite baseline from this repo state:

- Blargg: `pass=15 fail=1 unsupported_or_error=0`
- Mooneye `acceptance`: confirmed current pass/fail set listed below from the latest run; the long tail still needs a full uninterrupted sweep for an exact total

### Blargg

Current confirmed failure:

- `oam_bug`

### Mooneye

Current confirmed failures in the observed portion of the latest run:

- `bits/unused_hwio-GS`
- `boot_div-S`
- `boot_div-dmg0`
- `boot_div-dmgABCmgb`
- `boot_div2-S`
- `boot_hwio-dmg0`
- `boot_hwio-dmgABCmgb`
- `di_timing-GS`
- `ei_sequence`
- `halt_ime1_timing2-GS`

Current confirmed passes include:

- `add_sp_e_timing`
- `bits/mem_oam`
- `bits/reg_f`
- `boot_hwio-S`
- `boot_regs-dmg0`
- `boot_regs-dmgABC`
- `boot_regs-mgb`
- `call_cc_timing`
- `call_cc_timing2`
- `call_timing`
- `call_timing2`
- `div_timing`
- `ei_timing`
- `halt_ime0_ei`
- `halt_ime0_nointr_timing`
- `halt_ime1_timing`
- `if_ie_registers`
- `instr/daa`
- `interrupts/ie_push`
- `intr_timing`
- `jp_cc_timing`
- `jp_timing`
- `ld_hl_sp_e_timing`
- `oam_dma/basic`
- `oam_dma/reg_read`
- `oam_dma/sources-GS`
- `oam_dma_restart`
- `oam_dma_start`
- `oam_dma_timing`
- `pop_timing`

## Current Frontier

The active frontier is no longer opcode coverage. It is startup-state accuracy, interrupt/HALT edge cases, remaining OAM behavior, and the larger PPU/LCD/STAT side.
