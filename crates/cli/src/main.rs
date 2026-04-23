use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gbbrain_core::{
    Breakpoint, DisassembledInstruction, FrameBuffer, Machine, MachineControl, MemoryRegion,
    RenderTarget, StopReason,
};
use gbbrain_gb::{DebugState, GbMachine, GbModel, TraceEntry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("serve") => run_stdio_server(),
        Some("mcp") => run_mcp_server(),
        Some("suite") => run_suite(&args[2..]),
        Some(path) => run_single_shot(path),
        None => {
            eprintln!(
                "usage: gbbrain <rom-path> | gbbrain serve | gbbrain mcp | gbbrain suite dmg [blargg|mooneye|all]"
            );
            ExitCode::from(2)
        }
    }
}

fn run_mcp_server() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    let mut state = SessionState::default();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(line) => line,
            Err(error) => {
                let _ = write_mcp_response(
                    &mut output,
                    None,
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32603, "message": format!("failed to read stdin: {error}") }
                    }),
                );
                return ExitCode::from(1);
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: McpRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_mcp_response(
                    &mut output,
                    None,
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32700, "message": format!("invalid JSON request: {error}") }
                    }),
                );
                continue;
            }
        };

        let should_shutdown = matches!(request.method.as_str(), "shutdown");
        let response = handle_mcp_request(&mut state, request);
        if write_mcp_response(&mut output, response.0, response.1).is_err() {
            return ExitCode::from(1);
        }

        if should_shutdown {
            return ExitCode::SUCCESS;
        }
    }

    ExitCode::SUCCESS
}

fn run_suite(args: &[String]) -> ExitCode {
    let platform = args.first().map(String::as_str).unwrap_or("dmg");
    let suite = args.get(1).map(String::as_str).unwrap_or("all");

    if platform != "dmg" {
        eprintln!("unsupported suite platform: {platform}");
        return ExitCode::from(2);
    }

    let mut roms = Vec::new();
    match suite {
        "blargg" => roms.extend(discover_blargg_dmg_roms()),
        "mooneye" => roms.extend(discover_mooneye_dmg_roms()),
        "all" => {
            roms.extend(discover_blargg_dmg_roms());
            roms.extend(discover_mooneye_dmg_roms());
        }
        _ => {
            eprintln!("unsupported suite selection: {suite}");
            return ExitCode::from(2);
        }
    }

    if roms.is_empty() {
        eprintln!("no matching ROMs found for suite '{suite}'");
        eprintln!(
            "expected Blargg under test-roms/blargg and Mooneye ROM binaries under test-roms/mooneye/build/acceptance"
        );
        return ExitCode::from(1);
    }

    let mut passed = 0_usize;
    let mut failed = 0_usize;
    let mut unsupported = 0_usize;

    for rom in roms {
        let result = run_dmg_test_rom(&rom);
        println!("{:<10} {}", result.status.label(), rom.display());
        if let Some(detail) = &result.detail {
            println!("  {detail}");
        }
        if !result.serial_output.is_empty() {
            println!("  serial: {}", result.serial_output.escape_default());
        }

        match result.status {
            SuiteStatus::Pass => passed += 1,
            SuiteStatus::Fail => failed += 1,
            SuiteStatus::Unsupported | SuiteStatus::Error => unsupported += 1,
        }
    }

    println!(
        "summary: pass={} fail={} unsupported_or_error={}",
        passed, failed, unsupported
    );

    if failed == 0 && unsupported == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_single_shot(rom_path: &str) -> ExitCode {
    let rom = match fs::read(rom_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("failed to read ROM '{}': {error}", rom_path);
            return ExitCode::from(1);
        }
    };

    let mut machine = match GbMachine::new(rom) {
        Ok(machine) => machine,
        Err(error) => {
            eprintln!("failed to create machine: {error}");
            return ExitCode::from(1);
        }
    };

    let step = match machine.step_instruction() {
        Ok(step) => step,
        Err(error) => {
            eprintln!("failed to execute instruction: {error}");
            return ExitCode::from(1);
        }
    };

    let snapshot = machine.snapshot();
    let rom_header = machine.inspect_memory(MemoryRegion::Rom, 0x100, 0x10);
    let ram_preview = machine.inspect_memory(MemoryRegion::Ram, 0, 8);
    let frame = machine.render_frame(RenderTarget::Main).ok();

    println!("platform: GB");
    println!("stop_reason: {:?}", step.stop_reason);
    println!("pc: 0x{:04x}", snapshot.registers.pc);
    println!("halted: {}", snapshot.halted);
    println!("instructions: {}", snapshot.instruction_counter);
    println!("rom_header_present: {}", rom_header.is_some());
    println!("ram_preview_present: {}", ram_preview.is_some());
    println!(
        "frame: {}",
        frame
            .map(|frame| format!("{}x{}", frame.width, frame.height))
            .unwrap_or_else(|| "unavailable".to_string())
    );

    ExitCode::SUCCESS
}

fn run_stdio_server() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    let mut state = SessionState::default();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(line) => line,
            Err(error) => {
                let _ = write_response(
                    &mut output,
                    None,
                    json!({ "ok": false, "error": format!("failed to read stdin: {error}") }),
                );
                return ExitCode::from(1);
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let envelope: RequestEnvelope = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                if write_response(
                    &mut output,
                    None,
                    json!({ "ok": false, "error": format!("invalid JSON request: {error}") }),
                )
                .is_err()
                {
                    return ExitCode::from(1);
                }
                continue;
            }
        };

        let should_shutdown = matches!(&envelope.request, Request::Shutdown);

        let response = match state.handle(envelope.request) {
            Ok(data) => json!({ "ok": true, "data": data }),
            Err(error) => json!({ "ok": false, "error": error }),
        };

        if write_response(&mut output, envelope.id, response).is_err() {
            return ExitCode::from(1);
        }

        if should_shutdown {
            return ExitCode::SUCCESS;
        }
    }

    ExitCode::SUCCESS
}

#[derive(Debug, Deserialize)]
struct McpRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn handle_mcp_request(state: &mut SessionState, request: McpRequest) -> (Option<Value>, Value) {
    let id = request.id;
    let response = match request.method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "gbbrain",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {}
                }
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "result": {
                "tools": mcp_tools()
            }
        }),
        "resources/list" => json!({
            "jsonrpc": "2.0",
            "result": {
                "resources": [
                    {
                        "uri": "gbbrain://snapshot",
                        "name": "Current Snapshot",
                        "description": "Current machine snapshot and debug state",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "gbbrain://cartridge",
                        "name": "Cartridge Metadata",
                        "description": "Metadata for the loaded cartridge",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "gbbrain://trace",
                        "name": "Instruction Trace",
                        "description": "Recent instruction trace entries",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "gbbrain://serial",
                        "name": "Serial Output",
                        "description": "Captured serial output text",
                        "mimeType": "text/plain"
                    }
                ]
            }
        }),
        "resources/templates" => json!({
            "jsonrpc": "2.0",
            "result": {
                "resourceTemplates": [
                    {
                        "uriTemplate": "gbbrain://memory/{region}/{address}/{len}",
                        "name": "Memory Range",
                        "description": "Read a memory range from a named region",
                        "mimeType": "application/json"
                    },
                    {
                        "uriTemplate": "gbbrain://disasm/{address}/{count}",
                        "name": "Disassembly Range",
                        "description": "Disassemble code starting at an address",
                        "mimeType": "application/json"
                    }
                ]
            }
        }),
        "resources/read" => {
            let uri = request
                .params
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match read_mcp_resource(state, uri) {
                Ok(result) => json!({ "jsonrpc": "2.0", "result": result }),
                Err(error) => json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32602, "message": error }
                }),
            }
        }
        "prompts/list" => json!({
            "jsonrpc": "2.0",
            "result": {
                "prompts": [
                    {
                        "name": "title_screen_probe",
                        "description": "Probe a commercial ROM title screen using the current debug surface"
                    }
                ]
            }
        }),
        "prompts/get" => {
            let name = request
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match name {
                "title_screen_probe" => json!({
                    "jsonrpc": "2.0",
                    "result": {
                        "description": "Use the emulator to reach the game's title screen and inspect the relevant frame.",
                        "messages": [
                            {
                                "role": "user",
                                "content": {
                                    "type": "text",
                                    "text": "Load the ROM, prefer a real boot ROM for DMG/MGB/SGB validation, run until the first meaningful frame or title-entry breakpoint, inspect the current snapshot, trace, serial output, and rendered frame, and report the smallest concrete hardware issue blocking title-screen capture. Do not chase tests."
                                }
                            }
                        ]
                    }
                }),
                _ => json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32602, "message": format!("unsupported prompt: {name}") }
                }),
            }
        }
        "tools/call" => {
            let name = request
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = request.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call_mcp_tool(state, name, arguments) {
                Ok(result) => mcp_tool_response(result),
                Err(error) => json!({
                    "jsonrpc": "2.0",
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": error
                        }],
                        "isError": true
                    }
                }),
            }
        }
        "shutdown" => json!({ "jsonrpc": "2.0", "result": { "shutdown": true } }),
        _ => json!({
            "jsonrpc": "2.0",
            "error": { "code": -32601, "message": format!("unknown method: {}", request.method) }
        }),
    };
    (id, response)
}

fn read_mcp_resource(state: &SessionState, uri: &str) -> Result<Value, String> {
    match uri {
        "gbbrain://snapshot" => {
            let machine = state.machine_ref()?;
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&SessionState::snapshot_dto(machine))
                        .unwrap_or_else(|_| "{}".to_string())
                }]
            }))
        }
        "gbbrain://cartridge" => {
            let machine = state.machine_ref()?;
            let info = machine.cartridge_info();
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&json!({
                        "title": info.title,
                        "type_code": info.type_code,
                        "has_battery": info.has_battery,
                        "has_rtc": info.has_rtc
                    })).unwrap_or_else(|_| "{}".to_string())
                }]
            }))
        }
        "gbbrain://trace" => {
            let machine = state.machine_ref()?;
            let trace: Vec<Value> = machine
                .trace_entries()
                .into_iter()
                .map(|entry| {
                    json!({
                        "instruction_counter": entry.instruction_counter,
                        "pc": entry.pc,
                        "opcode": entry.opcode,
                        "a": entry.a,
                        "f": entry.f,
                        "b": entry.b,
                        "c": entry.c,
                        "d": entry.d,
                        "e": entry.e,
                        "h": entry.h,
                        "l": entry.l,
                        "sp": entry.sp,
                        "stop_reason": stop_reason_name(entry.stop_reason)
                    })
                })
                .collect();
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&trace)
                        .unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        "gbbrain://serial" => {
            let machine = state.machine_ref()?;
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": String::from_utf8_lossy(machine.serial_output()).to_string()
                }]
            }))
        }
        _ if uri.starts_with("gbbrain://memory/") => {
            let (region, address, len) = parse_memory_uri(uri)?;
            let parsed_region = parse_memory_region(&region)?;
            let machine = state.machine_ref()?;
            let bytes = machine
                .inspect_memory(parsed_region, address, len)
                .ok_or_else(|| format!("requested memory range is unavailable: {uri}"))?;
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&json!({
                        "region": memory_region_name(parsed_region),
                        "address": address,
                        "len": bytes.len(),
                        "bytes": bytes
                    })).unwrap_or_else(|_| "{}".to_string())
                }]
            }))
        }
        _ if uri.starts_with("gbbrain://disasm/") => {
            let (address, count) = parse_disasm_uri(uri)?;
            let machine = state.machine_ref()?;
            let instructions: Vec<Value> = Machine::disassemble_range(machine, address, count)
                .into_iter()
                .map(|inst| {
                    json!({
                        "address": inst.address,
                        "bytes": inst.bytes,
                        "text": inst.text,
                        "len": inst.len
                    })
                })
                .collect();
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&instructions)
                        .unwrap_or_else(|_| "[]".to_string())
                }]
            }))
        }
        _ => Err(format!("unsupported resource uri: {uri}")),
    }
}

fn parse_memory_uri(uri: &str) -> Result<(String, u32, usize), String> {
    let parts: Vec<&str> = uri.trim_start_matches("gbbrain://memory/").split('/').collect();
    if parts.len() != 3 {
        return Err(format!("invalid memory resource uri: {uri}"));
    }
    let region = parts[0].to_string();
    let address = parts[1]
        .parse::<u32>()
        .map_err(|_| format!("invalid memory address in uri: {uri}"))?;
    let len = parts[2]
        .parse::<usize>()
        .map_err(|_| format!("invalid memory length in uri: {uri}"))?;
    Ok((region, address, len))
}

fn parse_disasm_uri(uri: &str) -> Result<(u16, usize), String> {
    let parts: Vec<&str> = uri.trim_start_matches("gbbrain://disasm/").split('/').collect();
    if parts.len() != 2 {
        return Err(format!("invalid disassembly resource uri: {uri}"));
    }
    let address = parts[0]
        .parse::<u16>()
        .map_err(|_| format!("invalid disassembly address in uri: {uri}"))?;
    let count = parts[1]
        .parse::<usize>()
        .map_err(|_| format!("invalid disassembly count in uri: {uri}"))?;
    Ok((address, count))
}

fn mcp_tool_response(result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": {
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            }]
        }
    })
}

fn write_mcp_response(output: &mut impl Write, id: Option<Value>, payload: Value) -> io::Result<()> {
    let mut response = payload;
    if let Some(id) = id {
        response["id"] = id;
    }
    serde_json::to_writer(&mut *output, &response)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn mcp_tools() -> Value {
    json!([
        tool_spec("load_rom", "Load a Game Boy ROM into the emulator", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "model": {"type": "string"},
                "bootrom_path": {"type": "string"},
                "cart_state_path": {"type": "string"}
            },
            "required": ["path"]
        })),
        tool_spec("snapshot", "Return a snapshot of the current machine state", json!({"type":"object","properties":{}})),
        tool_spec("step", "Step the machine by N instructions", json!({
            "type": "object",
            "properties": {"count": {"type": "integer", "minimum": 1}}
        })),
        tool_spec("run_for_cycles", "Run until a cycle budget is exhausted", json!({
            "type": "object",
            "properties": {"cycles": {"type": "integer", "minimum": 1}},
            "required": ["cycles"]
        })),
        tool_spec("run_for_frames", "Run until a frame budget is exhausted", json!({
            "type": "object",
            "properties": {"count": {"type": "integer", "minimum": 1}},
            "required": ["count"]
        })),
        tool_spec("run", "Run until stop or instruction budget", json!({
            "type": "object",
            "properties": {"max_instructions": {"type": "integer", "minimum": 1}}
        })),
        tool_spec("inspect_memory", "Inspect a memory region", json!({
            "type": "object",
            "properties": {
                "region": {"type": "string"},
                "address": {"type": "integer", "minimum": 0},
                "len": {"type": "integer", "minimum": 0}
            },
            "required": ["region", "address", "len"]
        })),
        tool_spec("read_address", "Read a system address", json!({
            "type": "object",
            "properties": {"address": {"type": "integer", "minimum": 0}},
            "required": ["address"]
        })),
        tool_spec("write_address", "Write a system address", json!({
            "type": "object",
            "properties": {
                "address": {"type": "integer", "minimum": 0},
                "value": {"type": "integer", "minimum": 0}
            },
            "required": ["address", "value"]
        })),
        tool_spec("set_input", "Set pressed buttons", json!({
            "type": "object",
            "properties": {"buttons": {"type": "array", "items": {"type": "string"}}}
        })),
        tool_spec("get_input", "Get pressed buttons", json!({"type":"object","properties":{}})),
        tool_spec("disassemble", "Disassemble a range of code", json!({
            "type": "object",
            "properties": {
                "address": {"type": "integer", "minimum": 0},
                "count": {"type": "integer", "minimum": 1}
            },
            "required": ["address"]
        })),
        tool_spec("save_snapshot", "Serialize the full machine state", json!({"type":"object","properties":{}})),
        tool_spec("load_snapshot", "Restore the full machine state", json!({
            "type": "object",
            "properties": {"bytes_base64": {"type": "string"}},
            "required": ["bytes_base64"]
        })),
        tool_spec("save_cart_state", "Serialize persistent cartridge state", json!({"type":"object","properties":{}})),
        tool_spec("load_cart_state", "Restore persistent cartridge state", json!({
            "type": "object",
            "properties": {"bytes_base64": {"type": "string"}},
            "required": ["bytes_base64"]
        })),
        tool_spec("save_cart_state_file", "Write persistent cartridge state to a file", json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })),
        tool_spec("load_cart_state_file", "Load persistent cartridge state from a file", json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })),
        tool_spec("export_save_ram", "Export cartridge save RAM", json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })),
        tool_spec("import_save_ram", "Import cartridge save RAM", json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })),
        tool_spec("add_breakpoint", "Add a breakpoint", json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string"},
                "address": {"type": "integer", "minimum": 0}
            },
            "required": ["kind", "address"]
        })),
        tool_spec("clear_breakpoints", "Clear all breakpoints", json!({"type":"object","properties":{}})),
        tool_spec("get_trace", "Return recent trace entries", json!({
            "type": "object",
            "properties": {"limit": {"type": "integer", "minimum": 1}}
        })),
        tool_spec("clear_trace", "Clear the trace buffer", json!({"type":"object","properties":{}})),
        tool_spec("get_serial_output", "Return captured serial output", json!({
            "type": "object",
            "properties": {"encoding": {"type": "string"}}
        })),
        tool_spec("clear_serial_output", "Clear captured serial output", json!({"type":"object","properties":{}})),
        tool_spec("render_frame", "Render the current frame buffer", json!({
            "type": "object",
            "properties": {
                "target": {"type": "string"},
                "encoding": {"type": "string"}
            }
        })),
        tool_spec("reset", "Reset the machine", json!({"type":"object","properties":{}})),
        tool_spec("cartridge_info", "Return loaded cartridge metadata", json!({"type":"object","properties":{}})),
        tool_spec("shutdown", "Shutdown the server", json!({"type":"object","properties":{}}))
    ])
}

fn tool_spec(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn call_mcp_tool(state: &mut SessionState, name: &str, arguments: Value) -> Result<Value, String> {
    match name {
        "load_rom" => {
            let path = required_string(&arguments, "path")?;
            let model = optional_string(&arguments, "model");
            let bootrom_path = optional_string(&arguments, "bootrom_path");
            let cart_state_path = optional_string(&arguments, "cart_state_path");
            state.load_rom(path, model, bootrom_path, cart_state_path)
        }
        "snapshot" => {
            let machine = state.machine_ref()?;
            Ok(json!({ "snapshot": SessionState::snapshot_dto(machine) }))
        }
        "step" => state.step(optional_u64(&arguments, "count").unwrap_or(1)),
        "run_for_cycles" => state.run_for_cycles(required_u64(&arguments, "cycles")?),
        "run_for_frames" => state.run_for_frames(required_u64(&arguments, "count")?),
        "run" => state.run(optional_u64(&arguments, "max_instructions")),
        "inspect_memory" => state.inspect_memory(
            required_string(&arguments, "region")?,
            required_u32(&arguments, "address")?,
            required_usize(&arguments, "len")?,
        ),
        "read_address" => state.read_address(required_u32(&arguments, "address")?),
        "write_address" => state.write_address(
            required_u32(&arguments, "address")?,
            required_u32(&arguments, "value")?,
        ),
        "set_input" => state.set_input(optional_string_array(&arguments, "buttons")),
        "get_input" => state.get_input(),
        "disassemble" => state.disassemble(
            required_u32(&arguments, "address")?,
            optional_usize(&arguments, "count").unwrap_or(8),
        ),
        "save_snapshot" => state.save_snapshot(),
        "load_snapshot" => state.load_snapshot(required_string(&arguments, "bytes_base64")?),
        "save_cart_state" => state.save_cart_state(),
        "load_cart_state" => state.load_cart_state(required_string(&arguments, "bytes_base64")?),
        "save_cart_state_file" => state.save_cart_state_file(required_string(&arguments, "path")?),
        "load_cart_state_file" => state.load_cart_state_file(required_string(&arguments, "path")?),
        "export_save_ram" => state.export_save_ram(required_string(&arguments, "path")?),
        "import_save_ram" => state.import_save_ram(required_string(&arguments, "path")?),
        "add_breakpoint" => {
            let kind = required_string(&arguments, "kind")?;
            let address = required_u32(&arguments, "address")?;
            state.add_breakpoint(&kind, address)
        }
        "clear_breakpoints" => {
            let machine = state.machine_mut()?;
            machine
                .clear_breakpoints()
                .map_err(|error| error.to_string())?;
            Ok(json!({"cleared": true}))
        }
        "get_trace" => state.get_trace(optional_usize(&arguments, "limit")),
        "clear_trace" => {
            let machine = state.machine_mut()?;
            machine.clear_trace();
            Ok(json!({"cleared": true}))
        }
        "get_serial_output" => state.get_serial_output(optional_string(&arguments, "encoding")),
        "clear_serial_output" => {
            let machine = state.machine_mut()?;
            machine.clear_serial_output();
            Ok(json!({"cleared": true}))
        }
        "render_frame" => state.render_frame(optional_string(&arguments, "target"), optional_string(&arguments, "encoding")),
        "reset" => {
            let machine = state.machine_mut()?;
            machine.reset().map_err(|error| error.to_string())?;
            Ok(json!({ "snapshot": SessionState::snapshot_dto(machine) }))
        }
        "cartridge_info" => state.cartridge_info(),
        "shutdown" => state.shutdown(),
        _ => Err(format!("unsupported tool: {name}")),
    }
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .ok_or_else(|| format!("missing string argument: {key}"))
}

fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_string_array(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn required_u32(arguments: &Value, key: &str) -> Result<u32, String> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("missing u32 argument: {key}"))
}

fn required_usize(arguments: &Value, key: &str) -> Result<usize, String> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("missing usize argument: {key}"))
}

fn optional_usize(arguments: &Value, key: &str) -> Option<usize> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn required_u64(arguments: &Value, key: &str) -> Result<u64, String> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing u64 argument: {key}"))
}

fn optional_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(Value::as_u64)
}

#[derive(Debug)]
struct SuiteResult {
    status: SuiteStatus,
    detail: Option<String>,
    serial_output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuiteStatus {
    Pass,
    Fail,
    Unsupported,
    Error,
}

impl SuiteStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unsupported => "UNSUPPORTED",
            Self::Error => "ERROR",
        }
    }
}

fn discover_blargg_dmg_roms() -> Vec<PathBuf> {
    let candidates = [
        "test-roms/blargg/cpu_instrs/individual/01-special.gb",
        "test-roms/blargg/cpu_instrs/individual/02-interrupts.gb",
        "test-roms/blargg/cpu_instrs/individual/03-op sp,hl.gb",
        "test-roms/blargg/cpu_instrs/individual/04-op r,imm.gb",
        "test-roms/blargg/cpu_instrs/individual/05-op rp.gb",
        "test-roms/blargg/cpu_instrs/individual/06-ld r,r.gb",
        "test-roms/blargg/cpu_instrs/individual/07-jr,jp,call,ret,rst.gb",
        "test-roms/blargg/cpu_instrs/individual/08-misc instrs.gb",
        "test-roms/blargg/cpu_instrs/individual/09-op r,r.gb",
        "test-roms/blargg/cpu_instrs/individual/10-bit ops.gb",
        "test-roms/blargg/cpu_instrs/individual/11-op a,(hl).gb",
        "test-roms/blargg/instr_timing/instr_timing.gb",
        "test-roms/blargg/mem_timing/mem_timing.gb",
        "test-roms/blargg/mem_timing-2/mem_timing.gb",
        "test-roms/blargg/halt_bug.gb",
        "test-roms/blargg/oam_bug/oam_bug.gb",
    ];

    candidates
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

fn discover_mooneye_dmg_roms() -> Vec<PathBuf> {
    let root = PathBuf::from("test-roms/mooneye/build/acceptance");
    let Ok(output) = std::process::Command::new("bash")
        .arg("-lc")
        .arg("find test-roms/mooneye/build/acceptance -type f -name '*.gb' 2>/dev/null | sort")
        .output()
    else {
        return Vec::new();
    };

    if !output.status.success() || !root.exists() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            !name.contains("-cgb")
                && !name.contains("-agb")
                && !name.contains("-ags")
                && !name.contains("-sgb")
                && !name.contains("-sgb2")
        })
        .collect()
}

fn run_dmg_test_rom(path: &Path) -> SuiteResult {
    const RUN_CHUNK: u64 = 4_096;
    const HALT_SPIN_LIMIT: usize = 50_000;

    let mut client = match StdioSession::spawn() {
        Ok(client) => client,
        Err(error) => {
            return SuiteResult {
                status: SuiteStatus::Error,
                detail: Some(format!("failed to start gbbrain serve: {error}")),
                serial_output: String::new(),
            };
        }
    };

    let model = infer_model_from_rom_path(path);
    let bootrom_path = resolve_bootrom_path(model);
    if let Err(error) = client.load_rom(path, model, bootrom_path.as_deref()) {
        return SuiteResult {
            status: SuiteStatus::Error,
            detail: Some(error),
            serial_output: String::new(),
        };
    }

    let is_mooneye = path
        .components()
        .any(|component| component.as_os_str() == "mooneye");
    if is_mooneye {
        if let Err(error) = client.add_breakpoint("opcode", 0x40) {
            return SuiteResult {
                status: SuiteStatus::Error,
                detail: Some(error),
                serial_output: String::new(),
            };
        }
    }

    let mut halt_spins = 0_usize;
    loop {
        let serial_output = match client.serial_output() {
            Ok(serial_output) => serial_output,
            Err(error) => {
                return SuiteResult {
                    status: SuiteStatus::Error,
                    detail: Some(error),
                    serial_output: String::new(),
                };
            }
        };

        if serial_output.contains("Passed") || serial_output.contains("passed") {
            return SuiteResult {
                status: SuiteStatus::Pass,
                detail: Some("detected Blargg serial success output".to_string()),
                serial_output,
            };
        }
        if serial_output.contains("Failed") || serial_output.contains("failed") {
            return SuiteResult {
                status: SuiteStatus::Fail,
                detail: Some("detected Blargg serial failure output".to_string()),
                serial_output,
            };
        }

        if !is_mooneye {
            match detect_blargg_ram_result(&mut client) {
                Ok(Some((status, detail))) => {
                    return SuiteResult {
                        status,
                        detail: Some(detail),
                        serial_output,
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return SuiteResult {
                        status: SuiteStatus::Error,
                        detail: Some(error),
                        serial_output,
                    };
                }
            }
        }

        match client.run(RUN_CHUNK) {
            Ok(outcome) => {
                if is_mooneye && outcome.stop_reason == "breakpoint_hit" {
                    match client.snapshot() {
                        Ok(snapshot) => {
                            if let Some(result) = detect_mooneye_result(&snapshot) {
                                return SuiteResult {
                                    status: result,
                                    detail: Some(
                                        "detected Mooneye pass/fail signature at opcode breakpoint"
                                            .to_string(),
                                    ),
                                    serial_output: client.serial_output().unwrap_or(serial_output),
                                };
                            }
                        }
                        Err(error) => {
                            return SuiteResult {
                                status: SuiteStatus::Error,
                                detail: Some(error),
                                serial_output,
                            };
                        }
                    }

                    if let Err(error) = client.clear_breakpoints() {
                        return SuiteResult {
                            status: SuiteStatus::Error,
                            detail: Some(error),
                            serial_output,
                        };
                    }
                    if let Err(error) = client.step(1) {
                        return SuiteResult {
                            status: SuiteStatus::Error,
                            detail: Some(error),
                            serial_output,
                        };
                    }
                    if let Err(error) = client.add_breakpoint("opcode", 0x40) {
                        return SuiteResult {
                            status: SuiteStatus::Error,
                            detail: Some(error),
                            serial_output,
                        };
                    }
                }
                if outcome.stop_reason == "halted" {
                    halt_spins += 1;
                    if halt_spins >= HALT_SPIN_LIMIT {
                        break;
                    }
                    if let Err(error) = client.run_for_cycles(RUN_CHUNK * 4) {
                        return SuiteResult {
                            status: SuiteStatus::Error,
                            detail: Some(error),
                            serial_output,
                        };
                    }
                    continue;
                }
                halt_spins = 0;
            }
            Err(error) => {
                return SuiteResult {
                    status: SuiteStatus::Unsupported,
                    detail: Some(error),
                    serial_output: client.serial_output().unwrap_or_default(),
                };
            }
        }
    }

    let serial_output = client.serial_output().unwrap_or_default();
    let detail = if serial_output.is_empty() {
        "no pass/fail signature before repeated halted state".to_string()
    } else {
        "execution stalled after repeated halted state without pass/fail signature".to_string()
    };

    SuiteResult {
        status: SuiteStatus::Fail,
        detail: Some(detail),
        serial_output,
    }
}

fn infer_model_from_rom_path(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.contains("-dmg0") {
        "dmg0"
    } else if name.contains("-mgb") {
        "mgb"
    } else if name.contains("-sgb2") {
        "sgb2"
    } else if name.contains("-sgb") {
        "sgb"
    } else if name.contains("-S") {
        "sgb"
    } else {
        "dmg"
    }
}

fn resolve_bootrom_path(model: &str) -> Option<PathBuf> {
    let key = match model {
        "dmg0" => "GBBRAIN_BOOTROM_DMG0",
        "dmg" => "GBBRAIN_BOOTROM_DMG",
        "mgb" => "GBBRAIN_BOOTROM_MGB",
        "sgb" => "GBBRAIN_BOOTROM_SGB",
        "sgb2" => "GBBRAIN_BOOTROM_SGB2",
        _ => return None,
    };
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.exists())
}

fn expected_bootrom_sha256(model: GbModel) -> &'static str {
    match model {
        GbModel::Dmg0 => "26e71cf01e301e5dc40e987cd2ecbf6d0276245890ac829db2a25323da86818e",
        GbModel::Dmg => "cf053eccb4ccafff9e67339d4e78e98dce7d1ed59be819d2a1ba2232c6fce1c7",
        GbModel::Mgb => "a8cb5f4f1f16f2573ed2ecd8daedb9c5d1dd2c30a481f9b179b5d725d95eafe2",
        GbModel::Sgb => "0e4ddff32fc9d1eeaae812a157dd246459b00c9e14f2f61751f661f32361e360",
        GbModel::Sgb2 => "fd243c4fb27008986316ce3df29e9cfbcdc0cd52704970555a8bb76edbec3988",
    }
}

fn synthetic_bootrom_warnings(rom: &[u8], bootrom_path: Option<&str>) -> Vec<String> {
    if bootrom_path.is_some() || rom.len() < 0x150 {
        return Vec::new();
    }

    let cartridge_type = rom[0x147];
    let rom_size = rom[0x148];
    let mut warnings = Vec::new();

    if matches!(cartridge_type, 0x01..=0x03) && rom_size == 0x05 {
        warnings.push(
            "synthetic boot ROM in use: 1 MiB MBC1 cartridges may be multicarts; use a real boot ROM if menu or game-selection behavior matters"
                .to_string(),
        );
    }

    warnings
}

fn default_cart_state_path_for_rom(rom_path: &Path) -> PathBuf {
    let file_name = rom_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cartridge");
    rom_path.with_file_name(format!("{file_name}.gbbrain-cart.json"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn detect_blargg_ram_result(
    client: &mut StdioSession,
) -> Result<Option<(SuiteStatus, String)>, String> {
    let bytes = client.inspect_memory("system", 0xA000, 64)?;
    if bytes.len() < 4 || bytes[1..4] != [0xDE, 0xB0, 0x61] {
        return Ok(None);
    }

    let status = bytes[0];
    if status == 0x80 {
        return Ok(None);
    }

    let text_end = bytes[4..]
        .iter()
        .position(|&byte| byte == 0)
        .map(|index| 4 + index)
        .unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[4..text_end])
        .trim()
        .to_string();

    let suite_status = if status == 0 {
        SuiteStatus::Pass
    } else {
        SuiteStatus::Fail
    };
    let detail = if text.is_empty() {
        format!("detected Blargg RAM result code {status}")
    } else {
        format!("detected Blargg RAM result code {status}: {text}")
    };

    Ok(Some((suite_status, detail)))
}

fn detect_mooneye_result(snapshot: &SnapshotDto) -> Option<SuiteStatus> {
    let pass = [
        snapshot.b, snapshot.c, snapshot.d, snapshot.e, snapshot.h, snapshot.l,
    ] == [3, 5, 8, 13, 21, 34];
    let fail = [
        snapshot.b, snapshot.c, snapshot.d, snapshot.e, snapshot.h, snapshot.l,
    ] == [0x42; 6];

    if pass {
        Some(SuiteStatus::Pass)
    } else if fail {
        Some(SuiteStatus::Fail)
    } else {
        None
    }
}

struct StdioSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

struct RunOutcome {
    stop_reason: String,
}

impl StdioSession {
    fn spawn() -> Result<Self, String> {
        let exe = env::current_exe()
            .map_err(|error| format!("failed to resolve current executable: {error}"))?;
        let mut child = Command::new(exe)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("failed to launch server process: {error}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "server process did not expose stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "server process did not expose stdout".to_string())?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn load_rom(
        &mut self,
        path: &Path,
        model: &str,
        bootrom_path: Option<&Path>,
    ) -> Result<(), String> {
        self.request(json!({
            "command": "load_rom",
            "path": path,
            "model": model,
            "bootrom_path": bootrom_path
        }))
        .map(|_| ())
    }

    fn snapshot(&mut self) -> Result<SnapshotDto, String> {
        let response = self.request(json!({ "command": "snapshot" }))?;
        serde_json::from_value::<SnapshotResponse>(response)
            .map(|response| response.snapshot)
            .map_err(|error| format!("invalid snapshot response: {error}"))
    }

    fn serial_output(&mut self) -> Result<String, String> {
        let response = self.request(json!({
            "command": "get_serial_output",
            "encoding": "text"
        }))?;
        serde_json::from_value::<SerialOutputResponse>(response)
            .map(|response| response.text)
            .map_err(|error| format!("invalid serial output response: {error}"))
    }

    fn inspect_memory(
        &mut self,
        region: &str,
        address: u32,
        len: usize,
    ) -> Result<Vec<u8>, String> {
        let response = self.request(json!({
            "command": "inspect_memory",
            "region": region,
            "address": address,
            "len": len
        }))?;
        serde_json::from_value::<InspectMemoryResponse>(response)
            .map(|response| response.bytes)
            .map_err(|error| format!("invalid inspect_memory response: {error}"))
    }

    fn step(&mut self, count: u64) -> Result<String, String> {
        let response = self.request(json!({
            "command": "step",
            "count": count
        }))?;
        serde_json::from_value::<RunResponse>(response)
            .map(|response| response.stop_reason)
            .map_err(|error| format!("invalid step response: {error}"))
    }

    fn run_for_cycles(&mut self, cycles: u64) -> Result<String, String> {
        let response = self.request(json!({
            "command": "run_for_cycles",
            "cycles": cycles
        }))?;
        serde_json::from_value::<RunResponse>(response)
            .map(|response| response.stop_reason)
            .map_err(|error| format!("invalid run_for_cycles response: {error}"))
    }

    fn add_breakpoint(&mut self, kind: &str, address: u32) -> Result<(), String> {
        self.request(json!({
            "command": "add_breakpoint",
            "kind": kind,
            "address": address
        }))
        .map(|_| ())
    }

    fn clear_breakpoints(&mut self) -> Result<(), String> {
        self.request(json!({ "command": "clear_breakpoints" }))
            .map(|_| ())
    }

    fn run(&mut self, max_instructions: u64) -> Result<RunOutcome, String> {
        let response = self.request(json!({
            "command": "run",
            "max_instructions": max_instructions
        }))?;
        serde_json::from_value::<RunResponse>(response)
            .map(|response| RunOutcome {
                stop_reason: response.stop_reason,
            })
            .map_err(|error| format!("invalid run response: {error}"))
    }

    fn shutdown(&mut self) -> Result<(), String> {
        self.request(json!({ "command": "shutdown" })).map(|_| ())
    }

    fn request(&mut self, request: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let mut envelope = request;
        envelope["id"] = json!(id);

        serde_json::to_writer(&mut self.stdin, &envelope)
            .map_err(|error| format!("failed to send request: {error}"))?;
        self.stdin
            .write_all(b"\n")
            .map_err(|error| format!("failed to terminate request: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("failed to flush request: {error}"))?;

        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("failed to read response: {error}"))?;

        if read == 0 {
            return Err("server process closed stdout".to_string());
        }

        let response: ResponseEnvelope = serde_json::from_str(&line)
            .map_err(|error| format!("invalid response JSON: {error}"))?;

        if response.id != Some(json!(id)) {
            return Err(format!(
                "response id mismatch: expected {id}, got {}",
                response
                    .id
                    .map(|value: Value| value.to_string())
                    .unwrap_or_else(|| "null".to_string())
            ));
        }

        if response.ok {
            response
                .data
                .ok_or_else(|| "server response omitted data".to_string())
        } else {
            Err(response
                .error
                .unwrap_or_else(|| "server returned an unspecified error".to_string()))
        }
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
        let _ = self.child.wait();
    }
}

fn write_response(output: &mut impl Write, id: Option<Value>, payload: Value) -> io::Result<()> {
    let mut response = payload;
    if let Some(id) = id {
        response["id"] = id;
    }
    serde_json::to_writer(&mut *output, &response)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[derive(Default)]
struct SessionState {
    machine: Option<GbMachine>,
    rom_path: Option<PathBuf>,
    cart_state_path: Option<PathBuf>,
}

impl SessionState {
    fn flush_cart_state_if_configured(&self) -> Result<(), String> {
        if let (Some(machine), Some(path)) = (&self.machine, &self.cart_state_path) {
            let bytes = Machine::save_cartridge_state(machine).map_err(|error| error.to_string())?;
            fs::write(path, bytes).map_err(|error| {
                format!(
                    "failed to write cartridge state '{}': {error}",
                    path.display()
                )
            })?;
        }
        Ok(())
    }

    fn snapshot_dto(machine: &GbMachine) -> SnapshotDto {
        let mut snapshot = SnapshotDto::from(machine.snapshot());
        snapshot.debug = DebugStateDto::from(machine.debug_state());
        snapshot
    }

    fn handle(&mut self, request: Request) -> Result<Value, String> {
        match request {
            Request::Ping => Ok(json!({ "message": "pong" })),
            Request::Help => Ok(json!({
                "commands": [
                    "ping",
                    "help",
                    "load_rom",
                    "cartridge_info",
                    "reset",
                    "step",
                    "run_for_cycles",
                    "run_for_frames",
                    "run_for_instructions",
                    "run",
                    "snapshot",
                    "inspect_memory",
                    "read_address",
                    "write_address",
                    "set_input",
                    "get_input",
                    "disassemble",
                    "save_snapshot",
                    "load_snapshot",
                    "save_cart_state",
                    "load_cart_state",
                    "save_cart_state_file",
                    "load_cart_state_file",
                    "export_save_ram",
                    "import_save_ram",
                    "add_breakpoint",
                    "clear_breakpoints",
                    "get_trace",
                    "clear_trace",
                    "get_serial_output",
                    "clear_serial_output",
                    "render_frame",
                    "shutdown"
                ],
                "breakpoint_kinds": ["pc", "opcode", "memory_read", "memory_write"],
                "models": ["dmg0", "dmg", "mgb", "sgb", "sgb2"]
            })),
            Request::LoadRom {
                path,
                model,
                bootrom_path,
                cart_state_path,
            } => self.load_rom(path, model, bootrom_path, cart_state_path),
            Request::CartridgeInfo => self.cartridge_info(),
            Request::Reset => {
                let machine = self.machine_mut()?;
                machine.reset().map_err(|error| error.to_string())?;
                Ok(json!({
                    "snapshot": Self::snapshot_dto(machine)
                }))
            }
            Request::Step { count } => self.step(count.unwrap_or(1)),
            Request::RunForCycles { cycles } => self.run_for_cycles(cycles),
            Request::RunForFrames { count } => self.run_for_frames(count),
            Request::RunForInstructions { count } => self.run_for_instructions(count),
            Request::Run { max_instructions } => self.run(max_instructions),
            Request::Snapshot => {
                let machine = self.machine_ref()?;
                Ok(json!({
                    "snapshot": Self::snapshot_dto(machine)
                }))
            }
            Request::Disassemble { address, count } => {
                self.disassemble(address, count.unwrap_or(8))
            }
            Request::SaveSnapshot => self.save_snapshot(),
            Request::LoadSnapshot { bytes_base64 } => self.load_snapshot(bytes_base64),
            Request::SaveCartState => self.save_cart_state(),
            Request::LoadCartState { bytes_base64 } => self.load_cart_state(bytes_base64),
            Request::SaveCartStateFile { path } => self.save_cart_state_file(path),
            Request::LoadCartStateFile { path } => self.load_cart_state_file(path),
            Request::ExportSaveRam { path } => self.export_save_ram(path),
            Request::ImportSaveRam { path } => self.import_save_ram(path),
            Request::InspectMemory {
                region,
                address,
                len,
            } => self.inspect_memory(region, address, len),
            Request::ReadAddress { address } => self.read_address(address),
            Request::WriteAddress { address, value } => self.write_address(address, value),
            Request::SetInput { buttons } => self.set_input(buttons),
            Request::GetInput => self.get_input(),
            Request::AddBreakpoint { kind, address } => self.add_breakpoint(&kind, address),
            Request::ClearBreakpoints => {
                let machine = self.machine_mut()?;
                machine
                    .clear_breakpoints()
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "cleared": true }))
            }
            Request::GetTrace { limit } => self.get_trace(limit),
            Request::ClearTrace => {
                let machine = self.machine_mut()?;
                machine.clear_trace();
                Ok(json!({ "cleared": true }))
            }
            Request::GetSerialOutput { encoding } => self.get_serial_output(encoding),
            Request::ClearSerialOutput => {
                let machine = self.machine_mut()?;
                machine.clear_serial_output();
                Ok(json!({ "cleared": true }))
            }
            Request::RenderFrame { target, encoding } => self.render_frame(target, encoding),
            Request::Shutdown => self.shutdown(),
        }
    }

    fn load_rom(
        &mut self,
        path: String,
        model: Option<String>,
        bootrom_path: Option<String>,
        cart_state_path: Option<String>,
    ) -> Result<Value, String> {
        self.flush_cart_state_if_configured()?;

        let rom =
            fs::read(&path).map_err(|error| format!("failed to read ROM '{path}': {error}"))?;
        let model = match model {
            Some(name) => {
                GbModel::from_name(&name).ok_or_else(|| format!("unsupported model: {name}"))?
            }
            None => GbModel::Dmg,
        };
        let bootrom = match bootrom_path.as_ref() {
            Some(path) => {
                let bytes = fs::read(path)
                    .map_err(|error| format!("failed to read boot ROM '{path}': {error}"))?;
                let actual_hash = sha256_hex(&bytes);
                let expected_hash = expected_bootrom_sha256(model);
                if actual_hash != expected_hash {
                    return Err(format!(
                        "boot ROM hash does not match model {}: expected {}, got {}",
                        model.as_name(),
                        expected_hash,
                        actual_hash
                    ));
                }
                Some(bytes)
            }
            None => None,
        };
        let warnings = synthetic_bootrom_warnings(&rom, bootrom_path.as_deref());
        let mut machine = GbMachine::new_with_model_and_bootrom(rom, model, bootrom)
            .map_err(|error| error.to_string())?;
        let cartridge = machine.cartridge_info();
        let effective_cart_state_path = cart_state_path.as_ref().map(PathBuf::from).or_else(|| {
            if cartridge.has_battery || cartridge.has_rtc {
                Some(default_cart_state_path_for_rom(Path::new(&path)))
            } else {
                None
            }
        });
        if let Some(path) = effective_cart_state_path.as_ref() {
            if Path::new(path).exists() {
                let bytes = fs::read(path).map_err(|error| {
                    format!(
                        "failed to read cartridge state '{}': {error}",
                        path.display()
                    )
                })?;
                Machine::load_cartridge_state(&mut machine, &bytes).map_err(|error| {
                    format!(
                        "failed to load cartridge state '{}': {error}",
                        path.display()
                    )
                })?;
            }
        }
        self.machine = Some(machine);
        self.rom_path = Some(PathBuf::from(&path));
        self.cart_state_path = effective_cart_state_path.clone();
        let machine = self.machine.as_ref().expect("machine just loaded");
        let cartridge = machine.cartridge_info();
        let snapshot = Self::snapshot_dto(machine);

        Ok(json!({
            "platform": "gb",
            "model": machine.model_name(),
            "rom_path": path,
            "bootrom_path": bootrom_path,
            "cart_state_path": effective_cart_state_path,
            "warnings": warnings,
            "cartridge": cartridge,
            "snapshot": snapshot
        }))
    }

    fn cartridge_info(&self) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        Ok(json!(machine.cartridge_info()))
    }

    fn step(&mut self, count: u64) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;
        let mut last_reason = StopReason::StepComplete;

        for _ in 0..count {
            let result = machine
                .step_instruction()
                .map_err(|error| error.to_string())?;
            last_reason = result.stop_reason;
            if result.stop_reason != StopReason::StepComplete {
                break;
            }
        }

        Ok(json!({
            "stop_reason": stop_reason_name(last_reason),
            "instructions_retired": machine.snapshot().instruction_counter - start_instruction_counter,
            "watchpoint": machine.last_watchpoint().map(|(kind, address)| json!({
                "kind": kind,
                "address": address
            })),
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn run_for_instructions(&mut self, count: u64) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;
        let stop_reason = execute_for_instructions(machine, count)?;

        Ok(json!({
            "stop_reason": stop_reason_name(stop_reason),
            "instructions_retired": machine.snapshot().instruction_counter - start_instruction_counter,
            "watchpoint": machine.last_watchpoint().map(|(kind, address)| json!({
                "kind": kind,
                "address": address
            })),
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn run_for_cycles(&mut self, cycles: u64) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;
        let start_cycle_counter = machine.debug_state().cycle_counter;
        let stop_reason = machine
            .run_for_cycles(cycles)
            .map_err(|error| error.to_string())?
            .stop_reason;
        let stop_reason = if stop_reason == StopReason::RunLimitReached {
            "cycle_budget_exhausted"
        } else {
            stop_reason_name(stop_reason)
        };

        Ok(json!({
            "stop_reason": stop_reason,
            "cycles_elapsed": machine.debug_state().cycle_counter - start_cycle_counter,
            "instructions_retired": machine.snapshot().instruction_counter - start_instruction_counter,
            "watchpoint": machine.last_watchpoint().map(|(kind, address)| json!({
                "kind": kind,
                "address": address
            })),
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn run_for_frames(&mut self, count: u64) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;
        let start_cycle_counter = machine.debug_state().cycle_counter;
        let stop_reason = machine
            .run_for_frames(count)
            .map_err(|error| error.to_string())?
            .stop_reason;
        let stop_reason = if stop_reason == StopReason::RunLimitReached {
            "frame_budget_exhausted"
        } else {
            stop_reason_name(stop_reason)
        };

        Ok(json!({
            "stop_reason": stop_reason,
            "cycles_elapsed": machine.debug_state().cycle_counter - start_cycle_counter,
            "instructions_retired": machine.snapshot().instruction_counter - start_instruction_counter,
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn run(&mut self, max_instructions: Option<u64>) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;

        let stop_reason = if let Some(limit) = max_instructions {
            execute_for_instructions(machine, limit)?
        } else {
            machine
                .run()
                .map_err(|error| error.to_string())?
                .stop_reason
        };

        Ok(json!({
            "stop_reason": stop_reason_name(stop_reason),
            "instructions_retired": machine.snapshot().instruction_counter - start_instruction_counter,
            "watchpoint": machine.last_watchpoint().map(|(kind, address)| json!({
                "kind": kind,
                "address": address
            })),
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn inspect_memory(&self, region: String, address: u32, len: usize) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let region = parse_memory_region(&region)?;
        let bytes = machine
            .inspect_memory(region, address, len)
            .ok_or_else(|| "requested memory range is unavailable".to_string())?;

        Ok(json!({
            "region": memory_region_name(region),
            "address": address,
            "len": bytes.len(),
            "bytes": bytes
        }))
    }

    fn read_address(&mut self, address: u32) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let address =
            u16::try_from(address).map_err(|_| format!("address out of range: {address}"))?;
        let value = Machine::read_address(machine, address);

        Ok(json!({
            "address": address,
            "value": value
        }))
    }

    fn write_address(&mut self, address: u32, value: u32) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let address =
            u16::try_from(address).map_err(|_| format!("address out of range: {address}"))?;
        let value = u8::try_from(value).map_err(|_| format!("value out of range: {value}"))?;
        Machine::write_address(machine, address, value);
        let read_back = Machine::read_address(machine, address);

        Ok(json!({
            "address": address,
            "value": value,
            "read_back": read_back
        }))
    }

    fn disassemble(&self, address: u32, count: usize) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let address =
            u16::try_from(address).map_err(|_| format!("address out of range: {address}"))?;
        let instructions: Vec<DisassemblyDto> = Machine::disassemble_range(machine, address, count)
            .into_iter()
            .map(DisassemblyDto::from)
            .collect();
        Ok(json!({ "instructions": instructions }))
    }

    fn set_input(&mut self, buttons: Vec<String>) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let mut mask = 0u8;
        for button in buttons {
            let bit = match button.as_str() {
                "right" => 0,
                "left" => 1,
                "up" => 2,
                "down" => 3,
                "a" => 4,
                "b" => 5,
                "select" => 6,
                "start" => 7,
                _ => return Err(format!("unsupported button: {button}")),
            };
            mask |= 1 << bit;
        }
        machine.set_pressed_buttons_mask(mask);
        Ok(json!({
            "buttons": pressed_button_names_from_mask(machine.pressed_buttons_mask()),
            "p1": Machine::read_address(machine, 0xFF00)
        }))
    }

    fn get_input(&mut self) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        Ok(json!({
            "buttons": pressed_button_names_from_mask(machine.pressed_buttons_mask()),
            "p1": Machine::read_address(machine, 0xFF00)
        }))
    }

    fn save_snapshot(&self) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let bytes = machine.save_state().map_err(|error| error.to_string())?;
        Ok(json!({
            "format": "gbbrain.gb.state.v1+json",
            "bytes_base64": BASE64.encode(bytes)
        }))
    }

    fn load_snapshot(&mut self, bytes_base64: String) -> Result<Value, String> {
        let bytes = BASE64
            .decode(bytes_base64)
            .map_err(|error| format!("invalid base64 snapshot: {error}"))?;
        let machine = GbMachine::load_state(&bytes).map_err(|error| error.to_string())?;
        self.machine = Some(machine);
        let machine = self.machine.as_ref().expect("snapshot just loaded");
        Ok(json!({
            "platform": "gb",
            "cartridge": machine.cartridge_info(),
            "snapshot": Self::snapshot_dto(machine)
        }))
    }

    fn save_cart_state(&self) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let bytes = machine
            .save_cartridge_state()
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "format": "gbbrain.gb.cart_state.v1+json",
            "bytes_base64": BASE64.encode(bytes)
        }))
    }

    fn load_cart_state(&mut self, bytes_base64: String) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let bytes = BASE64
            .decode(bytes_base64)
            .map_err(|error| format!("invalid base64 cartridge state: {error}"))?;
        Machine::load_cartridge_state(machine, &bytes).map_err(|error| error.to_string())?;
        let cartridge = machine.cartridge_info();
        Ok(json!({
            "cartridge": cartridge
        }))
    }

    fn save_cart_state_file(&self, path: String) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let bytes = Machine::save_cartridge_state(machine).map_err(|error| error.to_string())?;
        fs::write(&path, bytes)
            .map_err(|error| format!("failed to write cartridge state '{path}': {error}"))?;
        let cartridge = machine.cartridge_info();
        Ok(json!({
            "path": path,
            "cartridge": cartridge
        }))
    }

    fn load_cart_state_file(&mut self, path: String) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let bytes = fs::read(&path)
            .map_err(|error| format!("failed to read cartridge state '{path}': {error}"))?;
        Machine::load_cartridge_state(machine, &bytes).map_err(|error| error.to_string())?;
        let cartridge = machine.cartridge_info();
        Ok(json!({
            "path": path,
            "cartridge": cartridge
        }))
    }

    fn export_save_ram(&self, path: String) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let bytes = Machine::save_cartridge_ram(machine);
        fs::write(&path, bytes)
            .map_err(|error| format!("failed to write save RAM '{path}': {error}"))?;
        let cartridge = machine.cartridge_info();
        Ok(json!({
            "path": path,
            "cartridge": cartridge
        }))
    }

    fn import_save_ram(&mut self, path: String) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let bytes = fs::read(&path)
            .map_err(|error| format!("failed to read save RAM '{path}': {error}"))?;
        Machine::load_cartridge_ram(machine, &bytes).map_err(|error| error.to_string())?;
        let cartridge = machine.cartridge_info();
        Ok(json!({
            "path": path,
            "cartridge": cartridge
        }))
    }

    fn shutdown(&mut self) -> Result<Value, String> {
        self.flush_cart_state_if_configured()
            .map_err(|error| format!("failed to flush cartridge state during shutdown: {error}"))?;

        if let Some(path) = &self.cart_state_path {
            return Ok(json!({
                "shutdown": true,
                "cart_state_path": path,
            }));
        }

        Ok(json!({ "shutdown": true }))
    }

    fn add_breakpoint(&mut self, kind: &str, address: u32) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let breakpoint = parse_breakpoint(kind, address)?;
        machine
            .add_breakpoint(breakpoint)
            .map_err(|error| error.to_string())?;

        Ok(json!({
            "kind": kind,
            "address": address
        }))
    }

    fn render_frame(
        &self,
        target: Option<String>,
        encoding: Option<String>,
    ) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let target = parse_render_target(target.as_deref().unwrap_or("main"))?;
        let encoding = encoding.unwrap_or_else(|| "base64".to_string());
        let frame = machine
            .render_frame(target)
            .map_err(|error| error.to_string())?;

        match encoding.as_str() {
            "base64" => Ok(json!({
                "target": "main",
                "format": "rgba8",
                "width": frame.width,
                "height": frame.height,
                "bytes_base64": BASE64.encode(frame.pixels_rgba8)
            })),
            "summary" => Ok(json!(FrameSummary::from(&frame))),
            _ => Err(format!("unsupported frame encoding: {encoding}")),
        }
    }

    fn get_trace(&self, limit: Option<usize>) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let trace = machine.trace_entries();
        let take = limit.unwrap_or(trace.len());
        let start = trace.len().saturating_sub(take);
        let entries: Vec<TraceEntryDto> = trace[start..]
            .iter()
            .copied()
            .map(TraceEntryDto::from)
            .collect();

        Ok(json!({
            "entries": entries
        }))
    }

    fn get_serial_output(&self, encoding: Option<String>) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let bytes = machine.serial_output();
        let encoding = encoding.unwrap_or_else(|| "text".to_string());

        match encoding.as_str() {
            "text" => Ok(json!({
                "encoding": "text",
                "text": String::from_utf8_lossy(bytes)
            })),
            "bytes" => Ok(json!({
                "encoding": "bytes",
                "bytes": bytes
            })),
            "base64" => Ok(json!({
                "encoding": "base64",
                "bytes_base64": BASE64.encode(bytes)
            })),
            _ => Err(format!("unsupported serial encoding: {encoding}")),
        }
    }

    fn machine_ref(&self) -> Result<&GbMachine, String> {
        self.machine
            .as_ref()
            .ok_or_else(|| "no ROM loaded".to_string())
    }

    fn machine_mut(&mut self) -> Result<&mut GbMachine, String> {
        self.machine
            .as_mut()
            .ok_or_else(|| "no ROM loaded".to_string())
    }
}

#[derive(Debug, Deserialize)]
struct RequestEnvelope {
    #[serde(default)]
    id: Option<Value>,
    #[serde(flatten)]
    request: Request,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Request {
    Ping,
    Help,
    LoadRom {
        path: String,
        model: Option<String>,
        bootrom_path: Option<String>,
        cart_state_path: Option<String>,
    },
    CartridgeInfo,
    Reset,
    Step {
        count: Option<u64>,
    },
    RunForCycles {
        cycles: u64,
    },
    RunForFrames {
        count: u64,
    },
    RunForInstructions {
        count: u64,
    },
    Run {
        max_instructions: Option<u64>,
    },
    Snapshot,
    InspectMemory {
        region: String,
        address: u32,
        len: usize,
    },
    ReadAddress {
        address: u32,
    },
    WriteAddress {
        address: u32,
        value: u32,
    },
    SetInput {
        #[serde(default)]
        buttons: Vec<String>,
    },
    GetInput,
    Disassemble {
        address: u32,
        count: Option<usize>,
    },
    SaveSnapshot,
    LoadSnapshot {
        bytes_base64: String,
    },
    SaveCartState,
    LoadCartState {
        bytes_base64: String,
    },
    SaveCartStateFile {
        path: String,
    },
    LoadCartStateFile {
        path: String,
    },
    ExportSaveRam {
        path: String,
    },
    ImportSaveRam {
        path: String,
    },
    AddBreakpoint {
        kind: String,
        address: u32,
    },
    ClearBreakpoints,
    GetTrace {
        limit: Option<usize>,
    },
    ClearTrace,
    GetSerialOutput {
        encoding: Option<String>,
    },
    ClearSerialOutput,
    RenderFrame {
        target: Option<String>,
        encoding: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotDto {
    pc: u32,
    sp: u32,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    e: u32,
    f: u32,
    h: u32,
    l: u32,
    halted: bool,
    instruction_counter: u64,
    debug: DebugStateDto,
}

#[derive(Debug, Deserialize)]
struct ResponseEnvelope {
    #[serde(default)]
    id: Option<Value>,
    ok: bool,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotResponse {
    snapshot: SnapshotDto,
}

#[derive(Debug, Deserialize)]
struct SerialOutputResponse {
    text: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RunResponse {
    stop_reason: String,
    #[serde(default)]
    watchpoint: Option<WatchpointResponse>,
    snapshot: SnapshotDto,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WatchpointResponse {
    kind: String,
    address: u32,
}

#[derive(Debug, Deserialize)]
struct InspectMemoryResponse {
    bytes: Vec<u8>,
}

impl From<gbbrain_core::MachineSnapshot> for SnapshotDto {
    fn from(snapshot: gbbrain_core::MachineSnapshot) -> Self {
        Self {
            pc: snapshot.registers.pc,
            sp: snapshot.registers.sp,
            a: snapshot.registers.a,
            b: snapshot.registers.b,
            c: snapshot.registers.c,
            d: snapshot.registers.d,
            e: snapshot.registers.e,
            f: snapshot.registers.f,
            h: snapshot.registers.h,
            l: snapshot.registers.l,
            halted: snapshot.halted,
            instruction_counter: snapshot.instruction_counter,
            debug: DebugStateDto::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct DebugStateDto {
    cycle_counter: u64,
    div_counter: u16,
    ppu_cycle_counter: u16,
    frame_counter: u64,
    ime: bool,
    ie: u8,
    if_reg: u8,
    lcdc: u8,
    stat: u8,
    ly: u8,
}

impl From<DebugState> for DebugStateDto {
    fn from(state: DebugState) -> Self {
        Self {
            cycle_counter: state.cycle_counter,
            div_counter: state.div_counter,
            ppu_cycle_counter: state.ppu_cycle_counter,
            frame_counter: state.frame_counter,
            ime: state.ime,
            ie: state.ie,
            if_reg: state.if_reg,
            lcdc: state.lcdc,
            stat: state.stat,
            ly: state.ly,
        }
    }
}

#[derive(Debug, Serialize)]
struct FrameSummary {
    target: &'static str,
    format: &'static str,
    width: u32,
    height: u32,
    byte_len: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct TraceEntryDto {
    instruction_counter: u64,
    pc: u16,
    opcode: u8,
    a: u8,
    f: u8,
    b: u8,
    c: u8,
    d: u8,
    e: u8,
    h: u8,
    l: u8,
    sp: u16,
    stop_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DisassemblyDto {
    address: u16,
    bytes: Vec<u8>,
    text: String,
    len: u8,
}

impl From<TraceEntry> for TraceEntryDto {
    fn from(entry: TraceEntry) -> Self {
        Self {
            instruction_counter: entry.instruction_counter,
            pc: entry.pc,
            opcode: entry.opcode,
            a: entry.a,
            f: entry.f,
            b: entry.b,
            c: entry.c,
            d: entry.d,
            e: entry.e,
            h: entry.h,
            l: entry.l,
            sp: entry.sp,
            stop_reason: stop_reason_name(entry.stop_reason).to_string(),
        }
    }
}

impl From<DisassembledInstruction> for DisassemblyDto {
    fn from(value: DisassembledInstruction) -> Self {
        Self {
            address: value.address,
            bytes: value.bytes,
            text: value.text,
            len: value.len,
        }
    }
}

impl From<&FrameBuffer> for FrameSummary {
    fn from(frame: &FrameBuffer) -> Self {
        Self {
            target: "main",
            format: "rgba8",
            width: frame.width,
            height: frame.height,
            byte_len: frame.pixels_rgba8.len(),
        }
    }
}

fn parse_memory_region(region: &str) -> Result<MemoryRegion, String> {
    match region {
        "rom" => Ok(MemoryRegion::Rom),
        "ram" => Ok(MemoryRegion::Ram),
        "vram" => Ok(MemoryRegion::Vram),
        "oam" => Ok(MemoryRegion::Oam),
        "system" => Ok(MemoryRegion::AddressSpace(
            gbbrain_core::AddressSpace::System,
        )),
        _ => Err(format!("unsupported memory region: {region}")),
    }
}

fn memory_region_name(region: MemoryRegion) -> &'static str {
    match region {
        MemoryRegion::Rom => "rom",
        MemoryRegion::Ram => "ram",
        MemoryRegion::Vram => "vram",
        MemoryRegion::Oam => "oam",
        MemoryRegion::AddressSpace(_) => "system",
    }
}

fn pressed_button_names_from_mask(mask: u8) -> Vec<&'static str> {
    let mut buttons = Vec::new();
    for (bit, name) in [
        (0, "right"),
        (1, "left"),
        (2, "up"),
        (3, "down"),
        (4, "a"),
        (5, "b"),
        (6, "select"),
        (7, "start"),
    ] {
        if mask & (1 << bit) != 0 {
            buttons.push(name);
        }
    }
    buttons
}

fn parse_breakpoint(kind: &str, address: u32) -> Result<Breakpoint, String> {
    match kind {
        "pc" => Ok(Breakpoint::ProgramCounter(address)),
        "opcode" => u8::try_from(address)
            .map(Breakpoint::Opcode)
            .map_err(|_| format!("opcode breakpoint out of range: {address}")),
        "memory_read" | "read" => Ok(Breakpoint::MemoryRead(address)),
        "memory_write" | "write" => Ok(Breakpoint::MemoryWrite(address)),
        _ => Err(format!("unsupported breakpoint kind: {kind}")),
    }
}

fn parse_render_target(target: &str) -> Result<RenderTarget, String> {
    match target {
        "main" => Ok(RenderTarget::Main),
        _ => Err(format!("unsupported render target: {target}")),
    }
}

fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::StepComplete => "step_complete",
        StopReason::BreakpointHit => "breakpoint_hit",
        StopReason::WatchpointHit => "watchpoint_hit",
        StopReason::Halted => "halted",
        StopReason::FrameComplete => "frame_complete",
        StopReason::RunLimitReached => "instruction_budget_exhausted",
    }
}

fn execute_for_instructions(machine: &mut GbMachine, count: u64) -> Result<StopReason, String> {
    for _ in 0..count {
        let result = machine
            .step_instruction()
            .map_err(|error| error.to_string())?;
        match result.stop_reason {
            StopReason::StepComplete | StopReason::Halted => {}
            other => return Ok(other),
        }
    }
    Ok(StopReason::RunLimitReached)
}
