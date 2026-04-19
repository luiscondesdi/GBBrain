use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gbbrain_core::{
    Breakpoint, FrameBuffer, Machine, MachineControl, MemoryRegion, RenderTarget, StopReason,
};
use gbbrain_gb::{GbMachine, TraceEntry};
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
        "test-roms/blargg/interrupt_time/interrupt_time.gb",
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
    const STEP_LIMIT: usize = 2_000_000;

    let rom = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return SuiteResult {
                status: SuiteStatus::Error,
                detail: Some(format!("failed to read ROM: {error}")),
                serial_output: String::new(),
            };
        }
    };

    let mut machine = match GbMachine::new(rom) {
        Ok(machine) => machine,
        Err(error) => {
            return SuiteResult {
                status: SuiteStatus::Error,
                detail: Some(error.to_string()),
                serial_output: String::new(),
            };
        }
    };

    for _ in 0..STEP_LIMIT {
        if let Some(result) = detect_mooneye_result(&machine) {
            return SuiteResult {
                status: result,
                detail: Some("detected Mooneye pass/fail signature".to_string()),
                serial_output: String::from_utf8_lossy(machine.serial_output()).into_owned(),
            };
        }

        let serial = String::from_utf8_lossy(machine.serial_output());
        if serial.contains("Passed") || serial.contains("passed") {
            return SuiteResult {
                status: SuiteStatus::Pass,
                detail: Some("detected Blargg serial success output".to_string()),
                serial_output: serial.into_owned(),
            };
        }
        if serial.contains("Failed") || serial.contains("failed") {
            return SuiteResult {
                status: SuiteStatus::Fail,
                detail: Some("detected Blargg serial failure output".to_string()),
                serial_output: serial.into_owned(),
            };
        }

        match machine.step_instruction() {
            Ok(result) => {
                if result.stop_reason == StopReason::Halted {
                    break;
                }
            }
            Err(error) => {
                return SuiteResult {
                    status: SuiteStatus::Unsupported,
                    detail: Some(error.to_string()),
                    serial_output: String::from_utf8_lossy(machine.serial_output()).into_owned(),
                };
            }
        }
    }

    let serial_output = String::from_utf8_lossy(machine.serial_output()).into_owned();
    let detail = if serial_output.is_empty() {
        "no pass/fail signature before step limit or halt".to_string()
    } else {
        "execution stopped without recognized pass/fail signature".to_string()
    };

    SuiteResult {
        status: SuiteStatus::Fail,
        detail: Some(detail),
        serial_output,
    }
}

fn detect_mooneye_result(machine: &GbMachine) -> Option<SuiteStatus> {
    let snapshot = machine.snapshot();
    let pc = snapshot.registers.pc;
    let opcode = machine
        .inspect_memory(MemoryRegion::AddressSpace(gbbrain_core::AddressSpace::System), pc, 1)
        .and_then(|bytes| bytes.first().copied())?;

    if opcode != 0x40 {
        return None;
    }

    let regs = &snapshot.registers;
    let pass = [regs.b, regs.c, regs.d, regs.e, regs.h, regs.l] == [3, 5, 8, 13, 21, 34];
    let fail = [regs.b, regs.c, regs.d, regs.e, regs.h, regs.l] == [0x42; 6];

    if pass {
        Some(SuiteStatus::Pass)
    } else if fail {
        Some(SuiteStatus::Fail)
    } else {
        None
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
                    "run",
                    "snapshot",
                    "inspect_memory",
                    "add_breakpoint",
                    "clear_breakpoints",
                    "get_trace",
                    "clear_trace",
                    "get_serial_output",
                    "clear_serial_output",
                    "render_frame",
                    "shutdown"
                ]
            })),
            Request::LoadRom { path } => self.load_rom(path),
            Request::Reset => {
                let machine = self.machine_mut()?;
                machine.reset().map_err(|error| error.to_string())?;
                Ok(json!({
                    "snapshot": SnapshotDto::from(machine.snapshot())
                }))
            }
            Request::Step { count } => self.step(count.unwrap_or(1)),
            Request::Run { max_instructions } => self.run(max_instructions),
            Request::Snapshot => {
                let machine = self.machine_ref()?;
                Ok(json!({
                    "snapshot": SnapshotDto::from(machine.snapshot())
                }))
            }
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
        let snapshot = machine.snapshot();
        self.machine = Some(machine);
        self.rom_path = Some(PathBuf::from(&path));

        Ok(json!({
            "platform": "gb",
            "rom_path": path,
            "snapshot": SnapshotDto::from(snapshot)
        }))
    }

    fn step(&mut self, count: u64) -> Result<Value, String> {
        let machine = self.machine_mut()?;
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
            "snapshot": SnapshotDto::from(machine.snapshot())
        }))
    }

    fn run(&mut self, max_instructions: Option<u64>) -> Result<Value, String> {
        let machine = self.machine_mut()?;

        let stop_reason = if let Some(limit) = max_instructions {
            let mut last_reason = StopReason::StepComplete;
            for _ in 0..limit {
                let result = machine
                    .step_instruction()
                    .map_err(|error| error.to_string())?;
                last_reason = result.stop_reason;
                if result.stop_reason != StopReason::StepComplete {
                    break;
                }
            }
            last_reason
        } else {
            machine.run().map_err(|error| error.to_string())?.stop_reason
        };

        Ok(json!({
            "stop_reason": stop_reason_name(stop_reason),
            "snapshot": SnapshotDto::from(machine.snapshot())
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
    Run { max_instructions: Option<u64> },
    Snapshot,
    InspectMemory { region: String, address: u32, len: usize },
    AddBreakpoint { kind: String, address: u32 },
    ClearBreakpoints,
    GetTrace { limit: Option<usize> },
    ClearTrace,
    GetSerialOutput { encoding: Option<String> },
    ClearSerialOutput,
    RenderFrame { target: Option<String>, encoding: Option<String> },
    Shutdown,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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
    stop_reason: &'static str,
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
            stop_reason: stop_reason_name(entry.stop_reason),
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
        "memory_read" => Ok(Breakpoint::MemoryRead(address)),
        "memory_write" => Ok(Breakpoint::MemoryWrite(address)),
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
        StopReason::RunLimitReached => "run_limit_reached",
    }
}
