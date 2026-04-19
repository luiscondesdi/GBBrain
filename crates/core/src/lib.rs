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
    fn snapshot(&self) -> MachineSnapshot;
    fn inspect_memory(&self, region: MemoryRegion, address: u32, len: usize) -> Option<Vec<u8>>;
    fn render_frame(&self, target: RenderTarget) -> Result<FrameBuffer, Self::Error>;
}

/// Runtime-control operations needed by agent loops.
pub trait MachineControl {
    type Error: std::error::Error + Send + Sync + 'static;

    fn reset(&mut self) -> Result<(), Self::Error>;
    fn run(&mut self) -> Result<RunResult, Self::Error>;
    fn step_instruction(&mut self) -> Result<RunResult, Self::Error>;
    fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), Self::Error>;
    fn clear_breakpoints(&mut self) -> Result<(), Self::Error>;
}

/// Serializable view of machine state at a stop point.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineSnapshot {
    pub registers: CpuRegisters,
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
}

/// Result of a run-control operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunResult {
    pub stop_reason: StopReason,
}

/// Breakpoint definitions for starter debugging flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    ProgramCounter(u32),
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
