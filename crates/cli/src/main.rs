use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gbbrain_core::{
    Breakpoint, FrameBuffer, Machine, MachineControl, MemoryRegion, RenderTarget, StopReason,
};
use gbbrain_gb::{DebugState, DisassembledInstruction, GbMachine, TraceEntry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("serve") => run_stdio_server(),
        Some("suite") => run_suite(&args[2..]),
        Some(path) => run_single_shot(path),
        None => {
            eprintln!("usage: gbbrain <rom-path> | gbbrain serve | gbbrain suite dmg [blargg|mooneye|all]");
            ExitCode::from(2)
        }
    }
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
        eprintln!("expected Blargg under test-roms/blargg and Mooneye ROM binaries under test-roms/mooneye/build/acceptance");
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
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
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

    if let Err(error) = client.load_rom(path) {
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

fn detect_blargg_ram_result(client: &mut StdioSession) -> Result<Option<(SuiteStatus, String)>, String> {
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
    let text = String::from_utf8_lossy(&bytes[4..text_end]).trim().to_string();

    let suite_status = if status == 0 { SuiteStatus::Pass } else { SuiteStatus::Fail };
    let detail = if text.is_empty() {
        format!("detected Blargg RAM result code {status}")
    } else {
        format!("detected Blargg RAM result code {status}: {text}")
    };

    Ok(Some((suite_status, detail)))
}

fn detect_mooneye_result(snapshot: &SnapshotDto) -> Option<SuiteStatus> {
    let pass = [snapshot.b, snapshot.c, snapshot.d, snapshot.e, snapshot.h, snapshot.l]
        == [3, 5, 8, 13, 21, 34];
    let fail = [snapshot.b, snapshot.c, snapshot.d, snapshot.e, snapshot.h, snapshot.l] == [0x42; 6];

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
        let exe = env::current_exe().map_err(|error| format!("failed to resolve current executable: {error}"))?;
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

    fn load_rom(&mut self, path: &Path) -> Result<(), String> {
        self.request(json!({
            "command": "load_rom",
            "path": path
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

    fn inspect_memory(&mut self, region: &str, address: u32, len: usize) -> Result<Vec<u8>, String> {
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

        let response: ResponseEnvelope =
            serde_json::from_str(&line).map_err(|error| format!("invalid response JSON: {error}"))?;

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

fn write_response(
    output: &mut impl Write,
    id: Option<Value>,
    payload: Value,
) -> io::Result<()> {
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
}

impl SessionState {
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
                    "reset",
                    "step",
                    "run_for_cycles",
                    "run_for_instructions",
                    "run",
                    "snapshot",
                    "inspect_memory",
                    "disassemble",
                    "save_snapshot",
                    "load_snapshot",
                    "add_breakpoint",
                    "clear_breakpoints",
                    "get_trace",
                    "clear_trace",
                    "get_serial_output",
                    "clear_serial_output",
                    "render_frame",
                    "shutdown"
                ],
                "breakpoint_kinds": ["pc", "opcode", "memory_read", "memory_write"]
            })),
            Request::LoadRom { path } => self.load_rom(path),
            Request::Reset => {
                let machine = self.machine_mut()?;
                machine.reset().map_err(|error| error.to_string())?;
                Ok(json!({
                    "snapshot": Self::snapshot_dto(machine)
                }))
            }
            Request::Step { count } => self.step(count.unwrap_or(1)),
            Request::RunForCycles { cycles } => self.run_for_cycles(cycles),
            Request::RunForInstructions { count } => self.run_for_instructions(count),
            Request::Run { max_instructions } => self.run(max_instructions),
            Request::Snapshot => {
                let machine = self.machine_ref()?;
                Ok(json!({
                    "snapshot": Self::snapshot_dto(machine)
                }))
            }
            Request::Disassemble { address, count } => self.disassemble(address, count.unwrap_or(8)),
            Request::SaveSnapshot => self.save_snapshot(),
            Request::LoadSnapshot { bytes_base64 } => self.load_snapshot(bytes_base64),
            Request::InspectMemory {
                region,
                address,
                len,
            } => self.inspect_memory(region, address, len),
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
            Request::Shutdown => Ok(json!({ "shutdown": true })),
        }
    }

    fn load_rom(&mut self, path: String) -> Result<Value, String> {
        let rom = fs::read(&path).map_err(|error| format!("failed to read ROM '{path}': {error}"))?;
        let machine = GbMachine::new(rom).map_err(|error| error.to_string())?;
        self.machine = Some(machine);
        self.rom_path = Some(PathBuf::from(&path));
        let snapshot = Self::snapshot_dto(self.machine.as_ref().expect("machine just loaded"));

        Ok(json!({
            "platform": "gb",
            "rom_path": path,
            "snapshot": snapshot
        }))
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
        let stop_reason = execute_for_cycles(machine, cycles)?;

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

    fn run(&mut self, max_instructions: Option<u64>) -> Result<Value, String> {
        let machine = self.machine_mut()?;
        let start_instruction_counter = machine.snapshot().instruction_counter;

        let stop_reason = if let Some(limit) = max_instructions {
            execute_for_instructions(machine, limit)?
        } else {
            machine.run().map_err(|error| error.to_string())?.stop_reason
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

    fn inspect_memory(
        &self,
        region: String,
        address: u32,
        len: usize,
    ) -> Result<Value, String> {
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

    fn disassemble(&self, address: u32, count: usize) -> Result<Value, String> {
        let machine = self.machine_ref()?;
        let address = u16::try_from(address).map_err(|_| format!("address out of range: {address}"))?;
        let instructions: Vec<DisassemblyDto> = machine
            .disassemble_range(address, count)
            .into_iter()
            .map(DisassemblyDto::from)
            .collect();
        Ok(json!({ "instructions": instructions }))
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
        Ok(json!({
            "platform": "gb",
            "snapshot": Self::snapshot_dto(self.machine.as_ref().expect("snapshot just loaded"))
        }))
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
        let frame = machine.render_frame(target).map_err(|error| error.to_string())?;

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
    LoadRom { path: String },
    Reset,
    Step { count: Option<u64> },
    RunForCycles { cycles: u64 },
    RunForInstructions { count: u64 },
    Run { max_instructions: Option<u64> },
    Snapshot,
    InspectMemory { region: String, address: u32, len: usize },
    Disassemble { address: u32, count: Option<usize> },
    SaveSnapshot,
    LoadSnapshot { bytes_base64: String },
    AddBreakpoint { kind: String, address: u32 },
    ClearBreakpoints,
    GetTrace { limit: Option<usize> },
    ClearTrace,
    GetSerialOutput { encoding: Option<String> },
    ClearSerialOutput,
    RenderFrame { target: Option<String>, encoding: Option<String> },
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
        "system" => Ok(MemoryRegion::AddressSpace(gbbrain_core::AddressSpace::System)),
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
        if result.stop_reason != StopReason::StepComplete {
            return Ok(result.stop_reason);
        }
    }
    Ok(StopReason::RunLimitReached)
}

fn execute_for_cycles(machine: &mut GbMachine, cycles: u64) -> Result<&'static str, String> {
    let start_cycles = machine.debug_state().cycle_counter;

    while machine.debug_state().cycle_counter.saturating_sub(start_cycles) < cycles {
        let result = machine
            .step_instruction()
            .map_err(|error| error.to_string())?;
        match result.stop_reason {
            StopReason::StepComplete | StopReason::Halted => {}
            other => return Ok(stop_reason_name(other)),
        }
    }

    Ok("cycle_budget_exhausted")
}
