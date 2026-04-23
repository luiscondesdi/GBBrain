# AI Interface

GBBrain exposes a machine-readable control surface over stdio.

Start the server with:

```bash
cargo run --bin gbbrain -- serve
```

The process reads newline-delimited JSON requests from stdin and writes newline-delimited JSON responses to stdout.

## Request Shape

Each request is a JSON object with a `command` field.

Optional:

- `id`: echoed back in the response so a client can correlate requests

Example:

```json
{"id":1,"command":"ping"}
```

## Response Shape

Successful responses:

```json
{"id":1,"ok":true,"data":{"message":"pong"}}
```

Failed responses:

```json
{"id":1,"ok":false,"error":"no ROM loaded"}
```

## Supported Commands

### `ping`

```json
{"id":1,"command":"ping"}
```

### `help`

Returns the current command list, supported breakpoint kinds, and supported machine models.

### `load_rom`

```json
{"id":2,"command":"load_rom","path":"/absolute/path/to/test.gb","model":"dmg"}
```

`model` is optional. Supported values are `dmg0`, `dmg`, `mgb`, `sgb`, and `sgb2`.

If omitted, the server defaults to `dmg`.

`bootrom_path` and `cart_state_path` are optional. If `cart_state_path` is provided and the file exists, cartridge-backed RAM/RTC state is loaded immediately after the ROM is created.
If `cart_state_path` is omitted for a battery-backed or RTC-backed cartridge, the server derives a default path next to the ROM named `<rom-filename>.gbbrain-cart.json`.
If a cartridge state path is active, the server also writes the current cartridge-backed RAM/RTC state back to that path on `shutdown`.

The response also includes cartridge metadata such as title, type code, battery presence, and RTC presence.

### `cartridge_info`

Returns metadata for the currently loaded cartridge.

```json
{"id":2,"command":"cartridge_info"}
```

### `reset`

Resets the currently loaded machine.

### `step`

Steps one or more instructions. This is the direct single-instruction execution primitive.

```json
{"id":3,"command":"step","count":4}
```

### `run_for_cycles`

Advances execution until either the cycle budget is exhausted or the machine hits a real stop condition.

```json
{"id":4,"command":"run_for_cycles","cycles":256}
```

### `run_for_instructions`

Advances execution until either the instruction budget is exhausted or the machine hits a real stop condition.

```json
{"id":5,"command":"run_for_instructions","count":64}
```

### `run`

Runs until a stop condition produced by the machine itself:

- breakpoint hit
- watchpoint hit
- halt
- internal run-limit guard

The intended usage is to set breakpoints or watchpoints first, then call `run`.

```json
{"id":6,"command":"run","max_instructions":1000}
```

### `snapshot`

Returns the current CPU register snapshot, halted state, instruction counter, and debug timing state.

### `inspect_memory`

Valid regions:

- `rom`
- `ram`
- `vram`
- `oam`
- `system`

```json
{"id":7,"command":"inspect_memory","region":"rom","address":256,"len":16}
```

### `read_address`

Reads one byte from the system address space.

```json
{"id":8,"command":"read_address","address":49152}
```

### `write_address`

Writes one byte into the system address space.

```json
{"id":9,"command":"write_address","address":49152,"value":66}
```

### `set_input`

Sets the currently pressed joypad buttons. Supported button names are:

- `right`
- `left`
- `up`
- `down`
- `a`
- `b`
- `select`
- `start`

```json
{"id":9,"command":"set_input","buttons":["start","a"]}
```

### `get_input`

Returns the currently pressed buttons and the current `FF00/P1` joypad register value.

```json
{"id":9,"command":"get_input"}
```

### `disassemble`

Returns decoded instructions for an address range.

```json
{"id":10,"command":"disassemble","address":256,"count":8}
```

### `save_snapshot`

Serializes the current machine state as base64.

```json
{"id":11,"command":"save_snapshot"}
```

### `load_snapshot`

Loads a previously saved machine state from base64.

```json
{"id":12,"command":"load_snapshot","bytes_base64":"..."}
```

### `save_cart_state`

Serializes persistent cartridge-backed state as base64. This includes external RAM and RTC state when present.

```json
{"id":12,"command":"save_cart_state"}
```

### `load_cart_state`

Loads previously saved persistent cartridge-backed state from base64.

```json
{"id":12,"command":"load_cart_state","bytes_base64":"..."}
```

### `save_cart_state_file`

Writes persistent cartridge-backed state directly to a file.

```json
{"id":12,"command":"save_cart_state_file","path":"/absolute/path/to/game.savstate.json"}
```

### `load_cart_state_file`

Loads persistent cartridge-backed state directly from a file.

```json
{"id":12,"command":"load_cart_state_file","path":"/absolute/path/to/game.savstate.json"}
```

### `export_save_ram`

Writes raw cartridge RAM bytes to a file for interoperability with standard `.sav` workflows.

```json
{"id":12,"command":"export_save_ram","path":"/absolute/path/to/game.sav"}
```

### `import_save_ram`

Loads raw cartridge RAM bytes from a file.

```json
{"id":12,"command":"import_save_ram","path":"/absolute/path/to/game.sav"}
```

### `add_breakpoint`

Valid kinds:

- `pc`
- `opcode`
- `memory_read`
- `memory_write`
- `read`
- `write`

```json
{"id":13,"command":"add_breakpoint","kind":"pc","address":257}
```

### `clear_breakpoints`

Clears all configured breakpoints and watchpoints.

### `get_trace`

Returns recent executed instructions from the in-memory trace buffer.

```json
{"id":14,"command":"get_trace","limit":32}
```

### `clear_trace`

Clears the current execution trace buffer.

### `get_serial_output`

Returns bytes written through the DMG serial test interface. This is especially useful for Blargg-style ROM output.

Valid encodings:

- `text`
- `bytes`
- `base64`

```json
{"id":15,"command":"get_serial_output","encoding":"text"}
```

### `clear_serial_output`

Clears the accumulated serial output buffer.

### `render_frame`

Valid encodings:

- `summary`
- `base64`

```json
{"id":16,"command":"render_frame","encoding":"summary"}
```

`base64` returns the full RGBA8 frame buffer inline. `summary` returns only metadata.

### `shutdown`

Acknowledges the request and exits the server process.

## Example Session

```json
{"id":1,"command":"load_rom","path":"/tmp/gbbrain-test.gb"}
{"id":2,"command":"add_breakpoint","kind":"opcode","address":64}
{"id":3,"command":"run","max_instructions":64}
{"id":4,"command":"get_trace","limit":8}
{"id":5,"command":"read_address","address":49152}
{"id":6,"command":"write_address","address":49152,"value":66}
{"id":7,"command":"save_snapshot"}
{"id":8,"command":"disassemble","address":256,"count":4}
{"id":9,"command":"get_serial_output","encoding":"text"}
{"id":10,"command":"render_frame","encoding":"summary"}
{"id":11,"command":"shutdown"}
```

## Current Limits

- Only the GB/DMG machine scaffold is exposed
- Hardware accuracy work is still in progress
- `render_frame` returns a deterministic debug frame, not a real LCD output yet
- The protocol is intentionally simple and not yet JSON-RPC
- The trace buffer is bounded to recent instructions only
