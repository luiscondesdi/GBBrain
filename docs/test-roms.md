# Test ROMs

GBBrain should use external Game Boy test ROMs during development, but those binaries must remain local and must not be committed to this repository.

## Recommended Suites

Use these suites first:

- Blargg CPU and timing tests
- Mooneye acceptance CPU and hardware behavior tests

## Local Layout

Keep downloaded ROMs in ignored directories such as:

- `roms/blargg/`
- `roms/mooneye/`
- `test-roms/blargg/`
- `test-roms/mooneye/`

Both `roms/` and `test-roms/` are git-ignored by this repository, and the top-level `*.gb`, `*.gbc`, and `*.gba` patterns are also ignored.

## Usage Policy

- Do not commit test ROM binaries.
- Do not vendor third-party test suites into the repo.
- Treat ROM paths as local developer configuration.
- Keep test harnesses flexible enough to point at a local ROM directory.

## Initial Focus

For the current DMG bootstrap, prioritize:

- Blargg `cpu_instrs`
- Blargg `instr_timing`
- Blargg `mem_timing`
- Mooneye `acceptance`

These should become the baseline external validation suites for CPU and memory behavior as the emulator core expands.
