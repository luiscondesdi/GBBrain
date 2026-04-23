//! Shared emulator primitives for GBBrain.

use core::fmt;

/// Platform families supported by the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Gb,
    Gbc,
    Gba,
}

/// Top-level machine interface used by agent-facing tools.
pub trait Machine {
    type Error: std::error::Error + Send + Sync + 'static;

    fn control(&mut self) -> &mut dyn MachineControl<Error = Self::Error>;
    fn model(&self) -> Platform;
    fn model_name(&self) -> &'static str;
    fn snapshot(&self) -> MachineSnapshot;
    fn debug_state(&self) -> DebugState;
    fn inspect_memory(&self, region: MemoryRegion, address: u32, len: usize) -> Option<Vec<u8>>;
    fn read_address(&mut self, address: u16) -> u8;
    fn write_address(&mut self, address: u16, value: u8);
    fn trace_entries(&self) -> Vec<TraceEntry>;
    fn clear_trace(&mut self);
    fn serial_output(&self) -> &[u8];
    fn clear_serial_output(&mut self);
    fn cartridge_info(&self) -> CartridgeInfo;
    fn pressed_buttons_mask(&self) -> u8;
    fn set_pressed_buttons_mask(&mut self, pressed: u8);
    fn last_watchpoint(&self) -> Option<WatchpointHit>;
    fn disassemble_range(&self, start: u16, count: usize) -> Vec<DisassembledInstruction>;
    fn save_cartridge_state(&self) -> Result<Vec<u8>, Self::Error>;
    fn load_cartridge_state(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn save_cartridge_ram(&self) -> Vec<u8>;
    fn load_cartridge_ram(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn render_frame(&self, target: RenderTarget) -> Result<FrameBuffer, Self::Error>;
}

/// Runtime-control operations needed by agent loops.
pub trait MachineControl {
    type Error: std::error::Error + Send + Sync + 'static;

    fn reset(&mut self) -> Result<(), Self::Error>;
    fn run(&mut self) -> Result<RunResult, Self::Error>;
    fn run_for_cycles(&mut self, cycles: u64) -> Result<RunResult, Self::Error>;
    fn run_for_frames(&mut self, count: u64) -> Result<RunResult, Self::Error>;
    fn step_instruction(&mut self) -> Result<RunResult, Self::Error>;
    fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), Self::Error>;
    fn clear_breakpoints(&mut self) -> Result<(), Self::Error>;
}

/// Serializable view of machine state at a stop point.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineSnapshot {
    pub registers: CpuRegisters,
    pub halted: bool,
    pub instruction_counter: u64,
}

/// Low-level machine timing/debug counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DebugState {
    pub cycle_counter: u64,
    pub div_counter: u16,
    pub ppu_cycle_counter: u16,
    pub frame_counter: u64,
    pub ppu_mode: u8,
    pub ime: bool,
    pub ie: u8,
    pub if_reg: u8,
    pub lcdc: u8,
    pub stat: u8,
    pub ly: u8,
}

/// CPU register file normalized across starter use cases.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuRegisters {
    pub pc: u32,
    pub sp: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
    pub e: u32,
    pub f: u32,
    pub h: u32,
    pub l: u32,
}

/// Addressable regions relevant to agent inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegion {
    Rom,
    Ram,
    Vram,
    Oam,
    AddressSpace(AddressSpace),
}

/// Full-system address spaces for future platform-specific handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpace {
    System,
}

/// Stop conditions surfaced after run/step operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    StepComplete,
    BreakpointHit,
    WatchpointHit,
    Halted,
    FrameComplete,
    RunLimitReached,
}

/// Result of a run-control operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunResult {
    pub stop_reason: StopReason,
}

/// Minimal instruction trace entry exposed through the shared API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceEntry {
    pub instruction_counter: u64,
    pub pc: u16,
    pub opcode: u8,
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub stop_reason: StopReason,
}

/// Cartridge metadata surfaced to tooling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CartridgeInfo {
    pub title: String,
    pub type_code: u8,
    pub has_battery: bool,
    pub has_rtc: bool,
}

/// Minimal disassembly entry exposed through the shared API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisassembledInstruction {
    pub address: u16,
    pub bytes: Vec<u8>,
    pub text: String,
    pub len: u8,
}

/// Most recent watchpoint hit, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchpointHit {
    pub kind: WatchpointKind,
    pub address: u16,
}

/// Watchpoint classification surfaced through the shared API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchpointKind {
    Read,
    Write,
}

/// Breakpoint definitions for starter debugging flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    ProgramCounter(u32),
    Opcode(u8),
    MemoryRead(u32),
    MemoryWrite(u32),
}

/// Render targets for systems with multiple display paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    Main,
}

/// Raw frame output for headless consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameBuffer {
    pub width: u32,
    pub height: u32,
    pub pixels_rgba8: Vec<u8>,
}

impl FrameBuffer {
    pub fn new_rgba(width: u32, height: u32) -> Self {
        let len = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or(0);

        Self {
            width,
            height,
            pixels_rgba8: vec![0; len],
        }
    }
}

/// Named register value exposed for structured inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterValue {
    pub name: &'static str,
    pub value: u32,
}

/// Shared error for placeholder implementations during bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnimplementedMachine;

impl fmt::Display for UnimplementedMachine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("machine behavior not implemented yet")
    }
}

impl std::error::Error for UnimplementedMachine {}
