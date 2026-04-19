# AI Interface

GBBrain exposes an MVP agent control surface over stdio.

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

Returns the current command list.

### `load_rom`

```json
{"id":2,"command":"load_rom","path":"/absolute/path/to/test.gb"}
```

### `reset`

Resets the currently loaded machine.

### `step`

Steps one or more instructions. This is the direct single-instruction execution primitive.

```json
{"id":3,"command":"step","count":4}
```

### `run`

Runs until a stop condition produced by the machine itself:

- breakpoint hit
- watchpoint hit
- halt
- internal run-limit guard

The intended usage is to set breakpoints or watchpoints first, then call `run`.

```json
{"id":4,"command":"run","max_instructions":1000}
```

### `snapshot`

Returns the current CPU register snapshot, halted state, and instruction counter.

### `inspect_memory`

Valid regions:

- `rom`
- `ram`
- `vram`
- `oam`
- `system`

```json
{"id":5,"command":"inspect_memory","region":"rom","address":256,"len":16}
```

### `add_breakpoint`

Valid kinds:

- `pc`
- `memory_read`
- `memory_write`

```json
{"id":6,"command":"add_breakpoint","kind":"pc","address":257}
```

### `clear_breakpoints`

Clears all configured breakpoints and watchpoints.

### `get_trace`

Returns recent executed instructions from the in-memory trace buffer.

```json
{"id":7,"command":"get_trace","limit":32}
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
{"id":8,"command":"get_serial_output","encoding":"text"}
```

### `clear_serial_output`

Clears the accumulated serial output buffer.

### `render_frame`

Valid encodings:

- `summary`
- `base64`

```json
{"id":7,"command":"render_frame","encoding":"summary"}
```

`base64` returns the full RGBA8 frame buffer inline. `summary` returns only metadata.

### `shutdown`

Acknowledges the request and exits the server process.

## Example Session

```json
{"id":1,"command":"load_rom","path":"/tmp/gbbrain-test.gb"}
{"id":2,"command":"add_breakpoint","kind":"pc","address":272}
{"id":3,"command":"run","max_instructions":64}
{"id":4,"command":"get_trace","limit":8}
{"id":5,"command":"get_serial_output","encoding":"text"}
{"id":6,"command":"render_frame","encoding":"summary"}
{"id":7,"command":"shutdown"}
```

## Current Limits

- Only the GB/DMG machine scaffold is exposed
- Opcode coverage is still minimal
- `render_frame` returns a deterministic debug frame, not a real LCD output yet
- The protocol is intentionally simple and not yet JSON-RPC
- The trace buffer is bounded to recent instructions only
