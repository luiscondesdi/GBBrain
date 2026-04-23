//! Minimal Game Boy DMG machine skeleton with usable debug plumbing.

use std::{collections::VecDeque, error::Error, fmt};

use gbbrain_core::{
    AddressSpace, Breakpoint, CpuRegisters, FrameBuffer, Machine, MachineControl, MachineSnapshot,
    MemoryRegion, RenderTarget, RunResult, StopReason,
};
use serde::{Deserialize, Serialize};

mod cartridge;
mod hardware;
mod state;
#[cfg(test)]
mod tests;
mod traits_impl;

use cartridge::Cartridge;

const ROM_BANK_0_END: u16 = 0x3FFF;
const ROM_BANK_N_END: u16 = 0x7FFF;
const VRAM_START: u16 = 0x8000;
const VRAM_END: u16 = 0x9FFF;
const WRAM_START: u16 = 0xC000;
const WRAM_END: u16 = 0xDFFF;
const ECHO_START: u16 = 0xE000;
const ECHO_END: u16 = 0xFDFF;
const OAM_START: u16 = 0xFE00;
const OAM_END: u16 = 0xFE9F;
const IO_START: u16 = 0xFF00;
const IO_END: u16 = 0xFF7F;
const SERIAL_SB: u16 = 0xFF01;
const SERIAL_SC: u16 = 0xFF02;
const DMA_REGISTER: u16 = 0xFF46;
const LCDC_REGISTER: u16 = 0xFF40;
const STAT_REGISTER: u16 = 0xFF41;
const LY_REGISTER: u16 = 0xFF44;
const TIMER_DIV: u16 = 0xFF04;
const TIMER_TIMA: u16 = 0xFF05;
const TIMER_TMA: u16 = 0xFF06;
const TIMER_TAC: u16 = 0xFF07;
const IF_REGISTER: u16 = 0xFF0F;
const HRAM_START: u16 = 0xFF80;
const HRAM_END: u16 = 0xFFFE;
const IE_REGISTER: u16 = 0xFFFF;
const DEFAULT_RUN_LIMIT: usize = 1_000_000;
const FRAME_WIDTH: u32 = 160;
const FRAME_HEIGHT: u32 = 144;
const TRACE_CAPACITY: usize = 512;
const INTERRUPT_VECTORS: &[(u8, u16)] = &[
    (0x01, 0x0040),
    (0x02, 0x0048),
    (0x04, 0x0050),
    (0x08, 0x0058),
    (0x10, 0x0060),
];
const PPU_ACCESS_OAM_CYCLES: u16 = 20;
const PPU_ACCESS_VRAM_CYCLES: u16 = 43;
const PPU_HBLANK_CYCLES: u16 = 50;
const PPU_VBLANK_LINE_CYCLES: u16 = 114;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bus {
    External,
    Internal,
    ExternalVideo,
    InternalVideo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OamCorruptionKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum PpuMode {
    HBlank = 0,
    VBlank = 1,
    AccessOam = 2,
    AccessVram = 3,
}

impl PpuMode {
    fn bits(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum TimaReloadState {
    OverflowDelay(u8),
    ReloadWindow(u8),
}

#[derive(Debug, Clone)]
pub enum GbError {
    EmptyRom,
    InvalidBootromSize(usize),
    InvalidRomLength(usize),
    UnsupportedCartridgeType(u8),
    UnsupportedRomSize(u8),
    UnsupportedRamSize(u8),
    RomSizeMismatch {
        header_size: usize,
        actual_size: usize,
    },
    MissingRamForCartridge(u8),
    UnexpectedRamSize {
        cartridge_type: u8,
        ram_size: u8,
    },
    PersistentStateRamSizeMismatch {
        expected: usize,
        actual: usize,
    },
    PersistentStateCartridgeMismatch,
    PersistentStateRtcMismatch,
    IllegalOpcode {
        opcode: u8,
        pc: u16,
    },
    StopInstruction {
        pc: u16,
    },
    StackOverflow(u16),
}

impl fmt::Display for GbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRom => f.write_str("ROM is empty"),
            Self::InvalidBootromSize(size) => {
                write!(f, "boot ROM must be exactly 256 bytes, got {size}")
            }
            Self::InvalidRomLength(size) => {
                write!(
                    f,
                    "ROM length must be a non-zero multiple of 16 KiB, got {size}"
                )
            }
            Self::UnsupportedCartridgeType(kind) => {
                write!(f, "unsupported cartridge type 0x{kind:02x}")
            }
            Self::UnsupportedRomSize(size) => {
                write!(f, "unsupported ROM size header 0x{size:02x}")
            }
            Self::UnsupportedRamSize(size) => {
                write!(f, "unsupported RAM size header 0x{size:02x}")
            }
            Self::RomSizeMismatch {
                header_size,
                actual_size,
            } => write!(
                f,
                "ROM size header expects {header_size} bytes, got {actual_size}"
            ),
            Self::MissingRamForCartridge(kind) => {
                write!(f, "cartridge type 0x{kind:02x} requires external RAM")
            }
            Self::UnexpectedRamSize {
                cartridge_type,
                ram_size,
            } => write!(
                f,
                "cartridge type 0x{cartridge_type:02x} should not declare RAM size 0x{ram_size:02x}"
            ),
            Self::PersistentStateRamSizeMismatch { expected, actual } => write!(
                f,
                "persistent cartridge RAM size mismatch: expected {expected} bytes, got {actual}"
            ),
            Self::PersistentStateCartridgeMismatch => {
                f.write_str("persistent cartridge state does not match loaded cartridge")
            }
            Self::PersistentStateRtcMismatch => {
                f.write_str("persistent cartridge RTC state does not match loaded cartridge")
            }
            Self::IllegalOpcode { opcode, pc } => {
                write!(f, "illegal opcode 0x{opcode:02x} at PC 0x{pc:04x}")
            }
            Self::StopInstruction { pc } => {
                write!(f, "STOP instruction encountered at PC 0x{pc:04x}")
            }
            Self::StackOverflow(address) => {
                write!(f, "stack access failed at address 0x{address:04x}")
            }
        }
    }
}

impl Error for GbError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Registers {
    a: u8,
    f: u8,
    b: u8,
    c: u8,
    d: u8,
    e: u8,
    h: u8,
    l: u8,
    sp: u16,
    pc: u16,
}

impl Registers {
    fn as_snapshot(self) -> CpuRegisters {
        CpuRegisters {
            pc: u32::from(self.pc),
            sp: u32::from(self.sp),
            a: u32::from(self.a),
            b: u32::from(self.b),
            c: u32::from(self.c),
            d: u32::from(self.d),
            e: u32::from(self.e),
            f: u32::from(self.f),
            h: u32::from(self.h),
            l: u32::from(self.l),
        }
    }

    fn set_hl(&mut self, value: u16) {
        self.h = (value >> 8) as u8;
        self.l = value as u8;
    }

    fn hl(self) -> u16 {
        (u16::from(self.h) << 8) | u16::from(self.l)
    }

    fn set_bc(&mut self, value: u16) {
        self.b = (value >> 8) as u8;
        self.c = value as u8;
    }

    fn bc(self) -> u16 {
        (u16::from(self.b) << 8) | u16::from(self.c)
    }

    fn set_de(&mut self, value: u16) {
        self.d = (value >> 8) as u8;
        self.e = value as u8;
    }

    fn de(self) -> u16 {
        (u16::from(self.d) << 8) | u16::from(self.e)
    }

    fn set_af(&mut self, value: u16) {
        self.a = (value >> 8) as u8;
        self.f = (value as u8) & 0xF0;
    }

    fn af(self) -> u16 {
        (u16::from(self.a) << 8) | u16::from(self.f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum WatchpointKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct WatchpointHit {
    kind: WatchpointKind,
    address: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
struct JoypadState {
    pressed: u8,
}

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

impl TraceEntry {
    fn from_state(
        instruction_counter: u64,
        pc: u16,
        opcode: u8,
        registers: Registers,
        stop_reason: StopReason,
    ) -> Self {
        Self {
            instruction_counter,
            pc,
            opcode,
            a: registers.a,
            f: registers.f,
            b: registers.b,
            c: registers.c,
            d: registers.d,
            e: registers.e,
            h: registers.h,
            l: registers.l,
            sp: registers.sp,
            stop_reason,
        }
    }
}

impl From<Breakpoint> for SnapshotBreakpoint {
    fn from(value: Breakpoint) -> Self {
        match value {
            Breakpoint::ProgramCounter(v) => Self::ProgramCounter(v),
            Breakpoint::Opcode(v) => Self::Opcode(v),
            Breakpoint::MemoryRead(v) => Self::MemoryRead(v),
            Breakpoint::MemoryWrite(v) => Self::MemoryWrite(v),
        }
    }
}

impl From<SnapshotBreakpoint> for Breakpoint {
    fn from(value: SnapshotBreakpoint) -> Self {
        match value {
            SnapshotBreakpoint::ProgramCounter(v) => Self::ProgramCounter(v),
            SnapshotBreakpoint::Opcode(v) => Self::Opcode(v),
            SnapshotBreakpoint::MemoryRead(v) => Self::MemoryRead(v),
            SnapshotBreakpoint::MemoryWrite(v) => Self::MemoryWrite(v),
        }
    }
}

impl From<TraceEntry> for SnapshotTraceEntry {
    fn from(value: TraceEntry) -> Self {
        Self {
            instruction_counter: value.instruction_counter,
            pc: value.pc,
            opcode: value.opcode,
            a: value.a,
            f: value.f,
            b: value.b,
            c: value.c,
            d: value.d,
            e: value.e,
            h: value.h,
            l: value.l,
            sp: value.sp,
            stop_reason: match value.stop_reason {
                StopReason::StepComplete => 0,
                StopReason::BreakpointHit => 1,
                StopReason::WatchpointHit => 2,
                StopReason::Halted => 3,
                StopReason::FrameComplete => 4,
                StopReason::RunLimitReached => 5,
            },
        }
    }
}

impl From<SnapshotTraceEntry> for TraceEntry {
    fn from(value: SnapshotTraceEntry) -> Self {
        Self {
            instruction_counter: value.instruction_counter,
            pc: value.pc,
            opcode: value.opcode,
            a: value.a,
            f: value.f,
            b: value.b,
            c: value.c,
            d: value.d,
            e: value.e,
            h: value.h,
            l: value.l,
            sp: value.sp,
            stop_reason: match value.stop_reason {
                0 => StopReason::StepComplete,
                1 => StopReason::BreakpointHit,
                2 => StopReason::WatchpointHit,
                3 => StopReason::Halted,
                4 => StopReason::FrameComplete,
                _ => StopReason::RunLimitReached,
            },
        }
    }
}

fn r8_name(index: u8) -> &'static str {
    match index {
        0 => "B",
        1 => "C",
        2 => "D",
        3 => "E",
        4 => "H",
        5 => "L",
        6 => "(HL)",
        7 => "A",
        _ => "?",
    }
}

fn cb_name(opcode: u8) -> String {
    let target = r8_name(opcode & 0x07);
    let bit = (opcode >> 3) & 0x07;
    match opcode {
        0x00..=0x07 => format!("RLC {target}"),
        0x08..=0x0F => format!("RRC {target}"),
        0x10..=0x17 => format!("RL {target}"),
        0x18..=0x1F => format!("RR {target}"),
        0x20..=0x27 => format!("SLA {target}"),
        0x28..=0x2F => format!("SRA {target}"),
        0x30..=0x37 => format!("SWAP {target}"),
        0x38..=0x3F => format!("SRL {target}"),
        0x40..=0x7F => format!("BIT {bit},{target}"),
        0x80..=0xBF => format!("RES {bit},{target}"),
        _ => format!("SET {bit},{target}"),
    }
}

/// Minimal DMG machine with a stable control and inspection surface.
pub struct GbMachine {
    cartridge: Cartridge,
    bootrom: Option<Box<[u8; 0x100]>>,
    bootrom_active: bool,
    model: GbModel,
    joypad: JoypadState,
    vram: [u8; 0x2000],
    wram: [u8; 0x2000],
    oam: [u8; 0xA0],
    io: [u8; 0x80],
    hram: [u8; 0x7F],
    ie: u8,
    registers: Registers,
    prefetched_pc: Option<u16>,
    prefetched_opcode: Option<u8>,
    breakpoints: Vec<Breakpoint>,
    ime: bool,
    exec_state: ExecState,
    instruction_counter: u64,
    cycle_counter: u64,
    div_counter: u16,
    tima_reload_state: Option<TimaReloadState>,
    ppu_cycle_counter: u16,
    ppu_mode: PpuMode,
    ppu_mode_cycles_remaining: u16,
    dma_source: u16,
    dma_active: bool,
    dma_requested_source: Option<u16>,
    dma_starting_source: Option<u16>,
    dma_next_byte: u16,
    pending_t34_interrupts: u8,
    pending_watchpoint: Option<WatchpointHit>,
    trace: VecDeque<TraceEntry>,
    serial_output: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct DebugState {
    pub cycle_counter: u64,
    pub div_counter: u16,
    pub ppu_cycle_counter: u16,
    pub ppu_mode: u8,
    pub ime: bool,
    pub ie: u8,
    pub if_reg: u8,
    pub lcdc: u8,
    pub stat: u8,
    pub ly: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotState {
    cartridge: Cartridge,
    #[serde(default)]
    bootrom: Option<Vec<u8>>,
    #[serde(default)]
    bootrom_active: bool,
    model: GbModel,
    #[serde(default)]
    joypad: JoypadState,
    vram: Vec<u8>,
    wram: Vec<u8>,
    oam: Vec<u8>,
    io: Vec<u8>,
    hram: Vec<u8>,
    ie: u8,
    registers: Registers,
    prefetched_pc: Option<u16>,
    prefetched_opcode: Option<u8>,
    breakpoints: Vec<SnapshotBreakpoint>,
    ime: bool,
    #[serde(default)]
    ime_enable_delay: u8,
    exec_state: ExecState,
    halted: bool,
    #[serde(default)]
    halt_bug: bool,
    instruction_counter: u64,
    cycle_counter: u64,
    div_counter: u16,
    tima_reload_state: Option<TimaReloadState>,
    ppu_cycle_counter: u16,
    #[serde(default = "default_ppu_mode")]
    ppu_mode: PpuMode,
    #[serde(default = "default_ppu_mode_cycles")]
    ppu_mode_cycles_remaining: u16,
    dma_source: u16,
    dma_active: bool,
    dma_requested_source: Option<u16>,
    dma_starting_source: Option<u16>,
    dma_next_byte: u16,
    #[serde(default)]
    pending_t34_interrupts: u8,
    pending_watchpoint: Option<WatchpointHit>,
    #[serde(default)]
    deferred_interrupt_flags: u8,
    #[serde(default)]
    deferred_interrupt_delay: u8,
    trace: Vec<SnapshotTraceEntry>,
    serial_output: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum SnapshotBreakpoint {
    ProgramCounter(u32),
    Opcode(u8),
    MemoryRead(u32),
    MemoryWrite(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisassembledInstruction {
    pub address: u16,
    pub bytes: Vec<u8>,
    pub text: String,
    pub len: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotTraceEntry {
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
    stop_reason: u8,
}

fn default_ppu_mode() -> PpuMode {
    PpuMode::HBlank
}

fn default_ppu_mode_cycles() -> u16 {
    PPU_ACCESS_OAM_CYCLES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ExecState {
    Running,
    Halt,
    Stop,
    InterruptDispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reg8Id {
    A,
    B,
    C,
    D,
    E,
    H,
    L,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddrMode8 {
    Bc,
    De,
    Hl,
    Hli,
    Hld,
    Direct,
    ZeroPageImm,
    ZeroPageC,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    Nz,
    Z,
    Nc,
    C,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackReg16 {
    Bc,
    De,
    Hl,
    Af,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reg16Pair {
    Bc,
    De,
    Hl,
    Sp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccumulatorOp {
    Rlca,
    Rrca,
    Rla,
    Rra,
    Daa,
    Cpl,
    Scf,
    Ccf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AluOp8 {
    Add,
    Adc,
    Sub,
    Sbc,
    And,
    Xor,
    Or,
    Cp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum In8 {
    Reg(Reg8Id),
    Addr(AddrMode8),
    Imm8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Out8 {
    Reg(Reg8Id),
    Addr(AddrMode8),
}

impl GbMachine {
    fn logical_pc(&self) -> u16 {
        self.prefetched_pc.unwrap_or(self.registers.pc)
    }

    fn logical_opcode(&self) -> u8 {
        self.prefetched_opcode
            .unwrap_or_else(|| self.peek8(self.registers.pc))
    }

    fn snapshot_registers(&self) -> CpuRegisters {
        let mut registers = self.registers;
        registers.pc = self.logical_pc();
        registers.as_snapshot()
    }

    fn prefetch_opcode_cycle(
        &mut self,
        address: u16,
        advance_pc: bool,
        allow_interrupt_dispatch: bool,
    ) {
        self.tick_timers(4);
        self.prefetched_pc = Some(address);
        self.prefetched_opcode = Some(self.read8(address));
        let sampled_interrupts = self.pending_interrupts();
        self.flush_pending_t34_interrupts();
        if allow_interrupt_dispatch && self.ime && sampled_interrupts != 0 {
            self.registers.pc = address;
            self.set_exec_state(ExecState::InterruptDispatch);
        } else {
            self.registers.pc = if advance_pc {
                address.wrapping_add(1)
            } else {
                address
            };
            self.set_exec_state(ExecState::Running);
        }
    }

    fn prefetch_next_cycle(&mut self, address: u16) {
        self.prefetch_opcode_cycle(address, true, true);
    }

    fn prefetch_no_interrupt_cycle(&mut self, address: u16) {
        self.prefetch_opcode_cycle(address, true, false);
    }

    fn prefetch_halt_bug_cycle(&mut self, address: u16) {
        self.prefetch_opcode_cycle(address, false, false);
    }

    fn consume_opcode(&mut self) -> (u16, u8) {
        if let (Some(pc), Some(opcode)) = (self.prefetched_pc.take(), self.prefetched_opcode.take())
        {
            (pc, opcode)
        } else {
            let opcode_pc = self.registers.pc;
            let opcode = self.fetch8_cycle();
            (opcode_pc, opcode)
        }
    }

    fn tick_mcycle(&mut self) {
        self.tick_timers(4);
        self.flush_pending_t34_interrupts();
    }

    fn fetch8_cycle(&mut self) -> u8 {
        self.tick_timers(4);
        let value = self.read8(self.registers.pc);
        self.flush_pending_t34_interrupts();
        self.registers.pc = self.registers.pc.wrapping_add(1);
        value
    }

    fn fetch8_timed_late(&mut self) -> u8 {
        self.fetch8_cycle()
    }

    fn fetch16_timed_late(&mut self) -> u16 {
        let lo = u16::from(self.fetch8_timed_late());
        let hi = u16::from(self.fetch8_timed_late());
        lo | (hi << 8)
    }

    fn read_cycle(&mut self, address: u16) -> u8 {
        self.tick_timers(4);
        let value = self.read8(address);
        self.flush_pending_t34_interrupts();
        value
    }

    fn write_cycle(&mut self, address: u16, value: u8) {
        self.tick_timers(4);
        self.write8(address, value);
        self.flush_pending_t34_interrupts();
    }

    fn write_cycle_intr(&mut self, address: u16, value: u8) -> u8 {
        self.tick_timers(4);
        self.write8(address, value);
        let sampled_interrupts = self.pending_interrupts();
        self.flush_pending_t34_interrupts();
        sampled_interrupts
    }

    fn read_r8_data(&mut self, index: u8) -> u8 {
        match index {
            0 => self.registers.b,
            1 => self.registers.c,
            2 => self.registers.d,
            3 => self.registers.e,
            4 => self.registers.h,
            5 => self.registers.l,
            6 => self.read8(self.registers.hl()),
            7 => self.registers.a,
            _ => 0xFF,
        }
    }

    fn read_hl_timed_late(&mut self) -> u8 {
        self.read_cycle(self.registers.hl())
    }

    fn write_hl_timed_late(&mut self, value: u8) {
        self.write_cycle(self.registers.hl(), value);
    }

    fn write_r8_data(&mut self, index: u8, value: u8) {
        match index {
            0 => self.registers.b = value,
            1 => self.registers.c = value,
            2 => self.registers.d = value,
            3 => self.registers.e = value,
            4 => self.registers.h = value,
            5 => self.registers.l = value,
            6 => self.write8(self.registers.hl(), value),
            7 => self.registers.a = value,
            _ => {}
        }
    }

    fn read_reg8(&mut self, reg: Reg8Id) -> u8 {
        match reg {
            Reg8Id::A => self.registers.a,
            Reg8Id::B => self.registers.b,
            Reg8Id::C => self.registers.c,
            Reg8Id::D => self.registers.d,
            Reg8Id::E => self.registers.e,
            Reg8Id::H => self.registers.h,
            Reg8Id::L => self.registers.l,
        }
    }

    fn write_reg8(&mut self, reg: Reg8Id, value: u8) {
        match reg {
            Reg8Id::A => self.registers.a = value,
            Reg8Id::B => self.registers.b = value,
            Reg8Id::C => self.registers.c = value,
            Reg8Id::D => self.registers.d = value,
            Reg8Id::E => self.registers.e = value,
            Reg8Id::H => self.registers.h = value,
            Reg8Id::L => self.registers.l = value,
        }
    }

    fn read_addr_mode8(&mut self, mode: AddrMode8) -> u8 {
        match mode {
            AddrMode8::Bc => self.read_cycle(self.registers.bc()),
            AddrMode8::De => self.read_cycle(self.registers.de()),
            AddrMode8::Hl => self.read_cycle(self.registers.hl()),
            AddrMode8::Hli => {
                let address = self.registers.hl();
                let value = self.read_cycle(address);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_add(1));
                value
            }
            AddrMode8::Hld => {
                let address = self.registers.hl();
                let value = self.read_cycle(address);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_sub(1));
                value
            }
            AddrMode8::Direct => {
                let address = self.fetch16_timed_late();
                self.read_cycle(address)
            }
            AddrMode8::ZeroPageImm => {
                let offset = self.fetch8_timed_late();
                self.read_cycle(0xFF00 | u16::from(offset))
            }
            AddrMode8::ZeroPageC => self.read_cycle(0xFF00 | u16::from(self.registers.c)),
        }
    }

    fn write_addr_mode8(&mut self, mode: AddrMode8, value: u8) {
        match mode {
            AddrMode8::Bc => self.write_cycle(self.registers.bc(), value),
            AddrMode8::De => self.write_cycle(self.registers.de(), value),
            AddrMode8::Hl => self.write_cycle(self.registers.hl(), value),
            AddrMode8::Hli => {
                let address = self.registers.hl();
                self.write_cycle(address, value);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_add(1));
            }
            AddrMode8::Hld => {
                let address = self.registers.hl();
                self.write_cycle(address, value);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_sub(1));
            }
            AddrMode8::Direct => {
                let address = self.fetch16_timed_late();
                self.write_cycle(address, value);
            }
            AddrMode8::ZeroPageImm => {
                let offset = self.fetch8_timed_late();
                self.write_cycle(0xFF00 | u16::from(offset), value);
            }
            AddrMode8::ZeroPageC => self.write_cycle(0xFF00 | u16::from(self.registers.c), value),
        }
    }

    fn read_in8(&mut self, operand: In8) -> u8 {
        match operand {
            In8::Reg(reg) => self.read_reg8(reg),
            In8::Addr(mode) => self.read_addr_mode8(mode),
            In8::Imm8 => self.fetch8_timed_late(),
        }
    }

    fn write_out8(&mut self, operand: Out8, value: u8) {
        match operand {
            Out8::Reg(reg) => self.write_reg8(reg, value),
            Out8::Addr(mode) => self.write_addr_mode8(mode, value),
        }
    }

    fn exec_load8(&mut self, dst: Out8, src: In8) {
        let value = self.read_in8(src);
        self.write_out8(dst, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_inc8(&mut self, operand: Out8) {
        let value = match operand {
            Out8::Reg(reg) => self.read_reg8(reg),
            Out8::Addr(mode) => self.read_addr_mode8(mode),
        };
        let result = self.inc8(value);
        self.write_out8(operand, result);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_dec8(&mut self, operand: Out8) {
        let value = match operand {
            Out8::Reg(reg) => self.read_reg8(reg),
            Out8::Addr(mode) => self.read_addr_mode8(mode),
        };
        let result = self.dec8(value);
        self.write_out8(operand, result);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_alu8<F>(&mut self, src: In8, mut op: F)
    where
        F: FnMut(&mut Self, u8),
    {
        let value = self.read_in8(src);
        op(self, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn set_flag(&mut self, mask: u8, enabled: bool) {
        if enabled {
            self.registers.f |= mask;
        } else {
            self.registers.f &= !mask;
        }
        self.registers.f &= 0xF0;
    }

    fn inc8(&mut self, value: u8) -> u8 {
        let result = value.wrapping_add(1);
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, (value & 0x0F) + 1 > 0x0F);
        result
    }

    fn dec8(&mut self, value: u8) -> u8 {
        let result = value.wrapping_sub(1);
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, true);
        self.set_flag(0x20, (value & 0x0F) == 0);
        result
    }

    fn add16_hl(&mut self, value: u16) {
        let hl = self.registers.hl();
        let result = hl.wrapping_add(value);
        self.set_flag(0x40, false);
        self.set_flag(0x20, ((hl & 0x0FFF) + (value & 0x0FFF)) > 0x0FFF);
        self.set_flag(0x10, (u32::from(hl) + u32::from(value)) > 0xFFFF);
        self.registers.set_hl(result);
    }

    fn inc_r16(&mut self, value: u16) -> u16 {
        self.maybe_trigger_oam_bug(value, OamCorruptionKind::Write);
        let result = value.wrapping_add(1);
        self.tick_mcycle();
        result
    }

    fn dec_r16(&mut self, value: u16) -> u16 {
        self.maybe_trigger_oam_bug(value, OamCorruptionKind::Write);
        let result = value.wrapping_sub(1);
        self.tick_mcycle();
        result
    }

    fn add_sp_signed(&mut self, value: i8) -> u16 {
        let sp = self.registers.sp;
        let value_u8 = value as u8;
        let result = sp.wrapping_add_signed(i16::from(value));
        self.set_flag(0x80, false);
        self.set_flag(0x40, false);
        self.set_flag(0x20, ((sp & 0x000F) + u16::from(value_u8 & 0x0F)) > 0x000F);
        self.set_flag(0x10, ((sp & 0x00FF) + u16::from(value_u8)) > 0x00FF);
        result
    }

    fn and8(&mut self, value: u8) {
        self.registers.a &= value;
        self.set_flag(0x80, self.registers.a == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, true);
        self.set_flag(0x10, false);
    }

    fn or8(&mut self, value: u8) {
        self.registers.a |= value;
        self.set_flag(0x80, self.registers.a == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, false);
    }

    fn xor8(&mut self, value: u8) {
        self.registers.a ^= value;
        self.set_flag(0x80, self.registers.a == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, false);
    }

    fn cp8(&mut self, value: u8) {
        let a = self.registers.a;
        let result = a.wrapping_sub(value);
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, true);
        self.set_flag(0x20, (a & 0x0F) < (value & 0x0F));
        self.set_flag(0x10, a < value);
    }

    fn sub8(&mut self, value: u8) {
        let a = self.registers.a;
        let result = a.wrapping_sub(value);
        self.registers.a = result;
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, true);
        self.set_flag(0x20, (a & 0x0F) < (value & 0x0F));
        self.set_flag(0x10, a < value);
    }

    fn sbc8(&mut self, value: u8) {
        let carry = u8::from(self.flag_c());
        let a = self.registers.a;
        let result = a.wrapping_sub(value).wrapping_sub(carry);
        self.registers.a = result;
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, true);
        self.set_flag(0x20, (a & 0x0F) < ((value & 0x0F) + carry));
        self.set_flag(0x10, u16::from(a) < (u16::from(value) + u16::from(carry)));
    }

    fn add8(&mut self, value: u8) {
        let a = self.registers.a;
        let result = a.wrapping_add(value);
        self.registers.a = result;
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, ((a & 0x0F) + (value & 0x0F)) > 0x0F);
        self.set_flag(0x10, (u16::from(a) + u16::from(value)) > 0xFF);
    }

    fn adc8(&mut self, value: u8) {
        let carry = u8::from(self.flag_c());
        let a = self.registers.a;
        let result = a.wrapping_add(value).wrapping_add(carry);
        self.registers.a = result;
        self.set_flag(0x80, result == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, ((a & 0x0F) + (value & 0x0F) + carry) > 0x0F);
        self.set_flag(
            0x10,
            (u16::from(a) + u16::from(value) + u16::from(carry)) > 0xFF,
        );
    }

    fn flag_z(&self) -> bool {
        self.registers.f & 0x80 != 0
    }

    fn flag_c(&self) -> bool {
        self.registers.f & 0x10 != 0
    }

    fn condition_true(&self, condition: Condition) -> bool {
        match condition {
            Condition::Nz => !self.flag_z(),
            Condition::Z => self.flag_z(),
            Condition::Nc => !self.flag_c(),
            Condition::C => self.flag_c(),
        }
    }

    fn decode_condition(bits: u8) -> Condition {
        match bits & 0x03 {
            0 => Condition::Nz,
            1 => Condition::Z,
            2 => Condition::Nc,
            _ => Condition::C,
        }
    }

    fn read_stack_reg16(&self, reg: StackReg16) -> u16 {
        match reg {
            StackReg16::Bc => self.registers.bc(),
            StackReg16::De => self.registers.de(),
            StackReg16::Hl => self.registers.hl(),
            StackReg16::Af => self.registers.af(),
        }
    }

    fn write_stack_reg16(&mut self, reg: StackReg16, value: u16) {
        match reg {
            StackReg16::Bc => self.registers.set_bc(value),
            StackReg16::De => self.registers.set_de(value),
            StackReg16::Hl => self.registers.set_hl(value),
            StackReg16::Af => self.registers.set_af(value),
        }
    }

    fn decode_stack_reg16(bits: u8) -> StackReg16 {
        match bits & 0x03 {
            0 => StackReg16::Bc,
            1 => StackReg16::De,
            2 => StackReg16::Hl,
            _ => StackReg16::Af,
        }
    }

    fn read_reg16_pair(&self, reg: Reg16Pair) -> u16 {
        match reg {
            Reg16Pair::Bc => self.registers.bc(),
            Reg16Pair::De => self.registers.de(),
            Reg16Pair::Hl => self.registers.hl(),
            Reg16Pair::Sp => self.registers.sp,
        }
    }

    fn write_reg16_pair(&mut self, reg: Reg16Pair, value: u16) {
        match reg {
            Reg16Pair::Bc => self.registers.set_bc(value),
            Reg16Pair::De => self.registers.set_de(value),
            Reg16Pair::Hl => self.registers.set_hl(value),
            Reg16Pair::Sp => self.registers.sp = value,
        }
    }

    fn decode_reg16_pair(bits: u8) -> Reg16Pair {
        match bits & 0x03 {
            0 => Reg16Pair::Bc,
            1 => Reg16Pair::De,
            2 => Reg16Pair::Hl,
            _ => Reg16Pair::Sp,
        }
    }

    fn decode_rst_vector(opcode: u8) -> u16 {
        u16::from(opcode & 0x38)
    }

    fn decode_accumulator_op(opcode: u8) -> Option<AccumulatorOp> {
        match opcode {
            0x07 => Some(AccumulatorOp::Rlca),
            0x0F => Some(AccumulatorOp::Rrca),
            0x17 => Some(AccumulatorOp::Rla),
            0x1F => Some(AccumulatorOp::Rra),
            0x27 => Some(AccumulatorOp::Daa),
            0x2F => Some(AccumulatorOp::Cpl),
            0x37 => Some(AccumulatorOp::Scf),
            0x3F => Some(AccumulatorOp::Ccf),
            _ => None,
        }
    }

    fn decode_alu_op(opcode: u8) -> AluOp8 {
        match (opcode >> 3) & 0x07 {
            0 => AluOp8::Add,
            1 => AluOp8::Adc,
            2 => AluOp8::Sub,
            3 => AluOp8::Sbc,
            4 => AluOp8::And,
            5 => AluOp8::Xor,
            6 => AluOp8::Or,
            _ => AluOp8::Cp,
        }
    }

    fn daa(&mut self) {
        let mut a = self.registers.a;
        let mut adjust = 0;
        let mut carry = self.flag_c();
        let n = self.registers.f & 0x40 != 0;
        let h = self.registers.f & 0x20 != 0;

        if !n {
            if carry || a > 0x99 {
                adjust |= 0x60;
                carry = true;
            }
            if h || (a & 0x0F) > 0x09 {
                adjust |= 0x06;
            }
            a = a.wrapping_add(adjust);
        } else {
            if carry {
                adjust |= 0x60;
            }
            if h {
                adjust |= 0x06;
            }
            a = a.wrapping_sub(adjust);
        }

        self.registers.a = a;
        self.set_flag(0x80, a == 0);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry);
    }

    fn ctrl_jp(&mut self, address: u16) {
        self.registers.pc = address;
        self.tick_mcycle();
    }

    fn ctrl_jr(&mut self, offset: i8) {
        self.registers.pc = self.registers.pc.wrapping_add_signed(i16::from(offset));
        self.tick_mcycle();
    }

    fn ctrl_call(&mut self, address: u16) -> Result<(), GbError> {
        self.tick_mcycle();
        self.push16(self.registers.pc)?;
        self.registers.pc = address;
        Ok(())
    }

    fn ctrl_ret(&mut self) {
        self.registers.pc = self.pop16();
        self.tick_mcycle();
    }

    fn ctrl_ret_cc(&mut self, condition: Condition) {
        self.tick_mcycle();
        if self.condition_true(condition) {
            self.ctrl_ret();
        }
    }

    fn ctrl_jp_cc(&mut self, condition: Condition, address: u16) {
        if self.condition_true(condition) {
            self.ctrl_jp(address);
        }
    }

    fn ctrl_call_cc(&mut self, condition: Condition, address: u16) -> Result<(), GbError> {
        if self.condition_true(condition) {
            self.ctrl_call(address)?;
        }
        Ok(())
    }

    fn push_reg16_prefetch(&mut self, value: u16, address: u16) -> Result<(), GbError> {
        self.tick_mcycle();
        self.push16(value)?;
        self.prefetch_next_cycle(address);
        Ok(())
    }

    fn pop_reg16_prefetch(&mut self) -> u16 {
        let value = self.pop16();
        self.prefetch_next_cycle(self.registers.pc);
        value
    }

    fn push_stack_reg16_prefetch(&mut self, reg: StackReg16) -> Result<(), GbError> {
        self.push_reg16_prefetch(self.read_stack_reg16(reg), self.registers.pc)
    }

    fn pop_stack_reg16_prefetch(&mut self, reg: StackReg16) {
        let value = self.pop_reg16_prefetch();
        self.write_stack_reg16(reg, value);
    }

    fn rst_to(&mut self, address: u16) -> Result<(), GbError> {
        self.push_reg16_prefetch(self.registers.pc, address)
    }

    fn ld_addr16_from_sp(&mut self, address: u16) {
        self.write_cycle(address, self.registers.sp as u8);
        self.write_cycle(address.wrapping_add(1), (self.registers.sp >> 8) as u8);
    }

    fn exec_ret_prefetch(&mut self) {
        self.ctrl_ret();
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_ret_condition_prefetch(&mut self, condition: Condition) {
        self.ctrl_ret_cc(condition);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_jp_immediate(&mut self) {
        let address = self.fetch16_timed_late();
        self.ctrl_jp(address);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_jp_condition_immediate(&mut self, condition: Condition) {
        let address = self.fetch16_timed_late();
        self.ctrl_jp_cc(condition, address);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_call_immediate(&mut self) -> Result<(), GbError> {
        let address = self.fetch16_timed_late();
        self.ctrl_call(address)?;
        self.prefetch_next_cycle(self.registers.pc);
        Ok(())
    }

    fn exec_call_condition_immediate(&mut self, condition: Condition) -> Result<(), GbError> {
        let address = self.fetch16_timed_late();
        self.ctrl_call_cc(condition, address)?;
        self.prefetch_next_cycle(self.registers.pc);
        Ok(())
    }

    fn exec_reti(&mut self) {
        self.ctrl_ret();
        self.ime = true;
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_load16_immediate(&mut self, reg: Reg16Pair) {
        let value = self.fetch16_timed_late();
        self.write_reg16_pair(reg, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_inc16(&mut self, reg: Reg16Pair) {
        let value = self.inc_r16(self.read_reg16_pair(reg));
        self.write_reg16_pair(reg, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_dec16(&mut self, reg: Reg16Pair) {
        let value = self.dec_r16(self.read_reg16_pair(reg));
        self.write_reg16_pair(reg, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_add_hl_reg16(&mut self, reg: Reg16Pair) {
        self.add16_hl(self.read_reg16_pair(reg));
        self.tick_mcycle();
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn add_sp_signed_prefetch(&mut self, offset: i8) {
        self.registers.sp = self.add_sp_signed(offset);
        self.tick_mcycle();
        self.tick_mcycle();
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn load_hl_sp_plus_signed_prefetch(&mut self, offset: i8) {
        let value = self.add_sp_signed(offset);
        self.registers.set_hl(value);
        self.tick_mcycle();
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn load_sp_from_hl_prefetch(&mut self) {
        self.registers.sp = self.registers.hl();
        self.tick_mcycle();
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn jr_cc_prefetch(&mut self, condition: bool) {
        let offset = self.fetch8_timed_late() as i8;
        if condition {
            self.ctrl_jr(offset);
        }
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_jr_condition(&mut self, condition: Condition) {
        self.jr_cc_prefetch(self.condition_true(condition));
    }

    fn exec_alu_op<F>(&mut self, src: In8, op: F)
    where
        F: FnMut(&mut Self, u8),
    {
        self.exec_alu8(src, op);
    }

    fn exec_decoded_alu(&mut self, src: In8, op: AluOp8) {
        match op {
            AluOp8::Add => self.exec_alu_op(src, Self::add8),
            AluOp8::Adc => self.exec_alu_op(src, Self::adc8),
            AluOp8::Sub => self.exec_alu_op(src, Self::sub8),
            AluOp8::Sbc => self.exec_alu_op(src, Self::sbc8),
            AluOp8::And => self.exec_alu_op(src, Self::and8),
            AluOp8::Xor => self.exec_alu_op(src, Self::xor8),
            AluOp8::Or => self.exec_alu_op(src, Self::or8),
            AluOp8::Cp => self.exec_alu_op(src, Self::cp8),
        }
    }

    fn rotate_a_left_circular(&mut self) {
        let carry = self.registers.a & 0x80 != 0;
        self.registers.a = self.registers.a.rotate_left(1);
        self.set_flag(0x80, false);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry);
    }

    fn rotate_a_right_circular(&mut self) {
        let carry = self.registers.a & 0x01 != 0;
        self.registers.a = self.registers.a.rotate_right(1);
        self.set_flag(0x80, false);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry);
    }

    fn rotate_a_left_through_carry(&mut self) {
        let carry_in = u8::from(self.flag_c());
        let carry_out = self.registers.a & 0x80 != 0;
        self.registers.a = (self.registers.a << 1) | carry_in;
        self.set_flag(0x80, false);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry_out);
    }

    fn rotate_a_right_through_carry(&mut self) {
        let carry_in = if self.flag_c() { 0x80 } else { 0 };
        let carry_out = self.registers.a & 0x01 != 0;
        self.registers.a = (self.registers.a >> 1) | carry_in;
        self.set_flag(0x80, false);
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry_out);
    }

    fn complement_a(&mut self) {
        self.registers.a = !self.registers.a;
        self.set_flag(0x40, true);
        self.set_flag(0x20, true);
    }

    fn complement_carry_flag(&mut self) {
        let carry = !self.flag_c();
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, carry);
    }

    fn set_carry_flag(&mut self) {
        self.set_flag(0x40, false);
        self.set_flag(0x20, false);
        self.set_flag(0x10, true);
    }

    fn exec_accumulator_op(&mut self, op: AccumulatorOp) {
        match op {
            AccumulatorOp::Rlca => self.rotate_a_left_circular(),
            AccumulatorOp::Rrca => self.rotate_a_right_circular(),
            AccumulatorOp::Rla => self.rotate_a_left_through_carry(),
            AccumulatorOp::Rra => self.rotate_a_right_through_carry(),
            AccumulatorOp::Daa => self.daa(),
            AccumulatorOp::Cpl => self.complement_a(),
            AccumulatorOp::Scf => self.set_carry_flag(),
            AccumulatorOp::Ccf => self.complement_carry_flag(),
        }
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn cb_read_operand(&mut self, target: u8) -> u8 {
        if target == 6 {
            self.read_hl_timed_late()
        } else {
            self.read_r8_data(target)
        }
    }

    fn cb_write_operand(&mut self, target: u8, value: u8) {
        if target == 6 {
            self.write_hl_timed_late(value);
        } else {
            self.write_r8_data(target, value);
        }
    }

    fn exec_cb_transform<F>(&mut self, target: u8, mut op: F)
    where
        F: FnMut(&mut Self, u8) -> u8,
    {
        let value = self.cb_read_operand(target);
        let result = op(self, value);
        self.cb_write_operand(target, result);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_cb_bit(&mut self, target: u8, bit: u8) {
        let value = self.cb_read_operand(target);
        self.set_flag(0x80, value & (1 << bit) == 0);
        self.set_flag(0x40, false);
        self.set_flag(0x20, true);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_cb_res(&mut self, target: u8, bit: u8) {
        let value = self.cb_read_operand(target) & !(1 << bit);
        self.cb_write_operand(target, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn exec_cb_set(&mut self, target: u8, bit: u8) {
        let value = self.cb_read_operand(target) | (1 << bit);
        self.cb_write_operand(target, value);
        self.prefetch_next_cycle(self.registers.pc);
    }

    fn decode_r8_in(index: u8) -> In8 {
        match index {
            0 => In8::Reg(Reg8Id::B),
            1 => In8::Reg(Reg8Id::C),
            2 => In8::Reg(Reg8Id::D),
            3 => In8::Reg(Reg8Id::E),
            4 => In8::Reg(Reg8Id::H),
            5 => In8::Reg(Reg8Id::L),
            6 => In8::Addr(AddrMode8::Hl),
            7 => In8::Reg(Reg8Id::A),
            _ => unreachable!("invalid r8 operand"),
        }
    }

    fn decode_r8_out(index: u8) -> Out8 {
        match index {
            0 => Out8::Reg(Reg8Id::B),
            1 => Out8::Reg(Reg8Id::C),
            2 => Out8::Reg(Reg8Id::D),
            3 => Out8::Reg(Reg8Id::E),
            4 => Out8::Reg(Reg8Id::H),
            5 => Out8::Reg(Reg8Id::L),
            6 => Out8::Addr(AddrMode8::Hl),
            7 => Out8::Reg(Reg8Id::A),
            _ => unreachable!("invalid r8 operand"),
        }
    }

    fn execute_cb_prefixed(&mut self) -> Result<(), GbError> {
        let opcode = self.fetch8_timed_late();
        let target = opcode & 0x07;
        match opcode {
            0x00..=0x07 => self.exec_cb_transform(target, |machine, value| {
                let carry = value & 0x80 != 0;
                let result = value.rotate_left(1);
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry);
                result
            }),
            0x08..=0x0F => self.exec_cb_transform(target, |machine, value| {
                let carry = value & 0x01 != 0;
                let result = value.rotate_right(1);
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry);
                result
            }),
            0x10..=0x17 => self.exec_cb_transform(target, |machine, value| {
                let carry_in = u8::from(machine.flag_c());
                let carry_out = value & 0x80 != 0;
                let result = (value << 1) | carry_in;
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry_out);
                result
            }),
            0x18..=0x1F => self.exec_cb_transform(target, |machine, value| {
                let carry_in = if machine.flag_c() { 0x80 } else { 0 };
                let carry_out = value & 0x01 != 0;
                let result = (value >> 1) | carry_in;
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry_out);
                result
            }),
            0x20..=0x27 => self.exec_cb_transform(target, |machine, value| {
                let carry = value & 0x80 != 0;
                let result = value << 1;
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry);
                result
            }),
            0x28..=0x2F => self.exec_cb_transform(target, |machine, value| {
                let carry = value & 0x01 != 0;
                let result = (value >> 1) | (value & 0x80);
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry);
                result
            }),
            0x30..=0x37 => self.exec_cb_transform(target, |machine, value| {
                let result = value.rotate_left(4);
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, false);
                result
            }),
            0x38..=0x3F => self.exec_cb_transform(target, |machine, value| {
                let carry = value & 0x01 != 0;
                let result = value >> 1;
                machine.set_flag(0x80, result == 0);
                machine.set_flag(0x40, false);
                machine.set_flag(0x20, false);
                machine.set_flag(0x10, carry);
                result
            }),
            0x40..=0x7F => self.exec_cb_bit(target, (opcode - 0x40) / 8),
            0x80..=0xBF => self.exec_cb_res(target, (opcode - 0x80) / 8),
            0xC0..=0xFF => self.exec_cb_set(target, (opcode - 0xC0) / 8),
        }
        Ok(())
    }

    fn execute_opcode_00_3f(&mut self, opcode: u8, _opcode_pc: u16) -> Result<(), GbError> {
        let reg16 = Self::decode_reg16_pair((opcode >> 4) & 0x03);
        match opcode {
            0x00 => self.prefetch_next_cycle(self.registers.pc),
            0x01 => self.exec_load16_immediate(reg16),
            0x02 => self.exec_load8(Out8::Addr(AddrMode8::Bc), In8::Reg(Reg8Id::A)),
            0x03 => self.exec_inc16(reg16),
            0x04 => self.exec_inc8(Out8::Reg(Reg8Id::B)),
            0x05 => self.exec_dec8(Out8::Reg(Reg8Id::B)),
            0x06 => self.exec_load8(Out8::Reg(Reg8Id::B), In8::Imm8),
            0x07 => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x08 => {
                let address = self.fetch16_timed_late();
                self.ld_addr16_from_sp(address);
                self.prefetch_next_cycle(self.registers.pc);
            }
            0x09 => self.exec_add_hl_reg16(reg16),
            0x0A => self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::Bc)),
            0x0B => self.exec_dec16(reg16),
            0x0C => self.exec_inc8(Out8::Reg(Reg8Id::C)),
            0x0D => self.exec_dec8(Out8::Reg(Reg8Id::C)),
            0x0E => self.exec_load8(Out8::Reg(Reg8Id::C), In8::Imm8),
            0x0F => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x10 => {
                let _ = self.fetch8_timed_late();
                self.set_exec_state(ExecState::Stop);
            }
            0x11 => self.exec_load16_immediate(reg16),
            0x12 => self.exec_load8(Out8::Addr(AddrMode8::De), In8::Reg(Reg8Id::A)),
            0x13 => self.exec_inc16(reg16),
            0x14 => self.exec_inc8(Out8::Reg(Reg8Id::D)),
            0x15 => self.exec_dec8(Out8::Reg(Reg8Id::D)),
            0x16 => self.exec_load8(Out8::Reg(Reg8Id::D), In8::Imm8),
            0x17 => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x18 => {
                let offset = self.fetch8_timed_late() as i8;
                self.ctrl_jr(offset);
                self.prefetch_next_cycle(self.registers.pc);
            }
            0x19 => self.exec_add_hl_reg16(reg16),
            0x1A => self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::De)),
            0x1B => self.exec_dec16(reg16),
            0x1C => self.exec_inc8(Out8::Reg(Reg8Id::E)),
            0x1D => self.exec_dec8(Out8::Reg(Reg8Id::E)),
            0x1E => self.exec_load8(Out8::Reg(Reg8Id::E), In8::Imm8),
            0x1F => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x20 => self.exec_jr_condition(Self::decode_condition((opcode >> 3) & 0x03)),
            0x21 => self.exec_load16_immediate(reg16),
            0x22 => self.exec_load8(Out8::Addr(AddrMode8::Hli), In8::Reg(Reg8Id::A)),
            0x23 => self.exec_inc16(reg16),
            0x24 => self.exec_inc8(Out8::Reg(Reg8Id::H)),
            0x25 => self.exec_dec8(Out8::Reg(Reg8Id::H)),
            0x26 => self.exec_load8(Out8::Reg(Reg8Id::H), In8::Imm8),
            0x27 => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x28 => self.exec_jr_condition(Self::decode_condition((opcode >> 3) & 0x03)),
            0x29 => self.exec_add_hl_reg16(reg16),
            0x2A => self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::Hli)),
            0x2B => self.exec_dec16(reg16),
            0x2C => self.exec_inc8(Out8::Reg(Reg8Id::L)),
            0x2D => self.exec_dec8(Out8::Reg(Reg8Id::L)),
            0x2E => self.exec_load8(Out8::Reg(Reg8Id::L), In8::Imm8),
            0x2F => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x30 => self.exec_jr_condition(Self::decode_condition((opcode >> 3) & 0x03)),
            0x31 => self.exec_load16_immediate(reg16),
            0x32 => self.exec_load8(Out8::Addr(AddrMode8::Hld), In8::Reg(Reg8Id::A)),
            0x33 => self.exec_inc16(reg16),
            0x34 => self.exec_inc8(Out8::Addr(AddrMode8::Hl)),
            0x35 => self.exec_dec8(Out8::Addr(AddrMode8::Hl)),
            0x36 => self.exec_load8(Out8::Addr(AddrMode8::Hl), In8::Imm8),
            0x37 => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            0x38 => self.exec_jr_condition(Self::decode_condition((opcode >> 3) & 0x03)),
            0x39 => self.exec_add_hl_reg16(reg16),
            0x3A => self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::Hld)),
            0x3B => self.exec_dec16(reg16),
            0x3C => self.exec_inc8(Out8::Reg(Reg8Id::A)),
            0x3D => self.exec_dec8(Out8::Reg(Reg8Id::A)),
            0x3E => self.exec_load8(Out8::Reg(Reg8Id::A), In8::Imm8),
            0x3F => self.exec_accumulator_op(Self::decode_accumulator_op(opcode).unwrap()),
            _ => unreachable!("unhandled opcode in 00-3F dispatcher: 0x{opcode:02X}"),
        }
        Ok(())
    }

    fn execute_opcode_40_bf(&mut self, opcode: u8, _opcode_pc: u16) -> Result<(), GbError> {
        match opcode {
            0x40..=0x7F if opcode != 0x76 => {
                let dst = Self::decode_r8_out((opcode >> 3) & 0x07);
                let src = Self::decode_r8_in(opcode & 0x07);
                self.exec_load8(dst, src);
            }
            0x76 => {
                if self.pending_interrupts() != 0 {
                    if self.ime {
                        self.prefetch_next_cycle(self.registers.pc);
                    } else {
                        self.prefetch_halt_bug_cycle(self.registers.pc);
                    }
                } else {
                    self.set_exec_state(ExecState::Halt);
                    self.tick_mcycle();
                }
            }
            0x80..=0xBF => self.exec_decoded_alu(
                Self::decode_r8_in(opcode & 0x07),
                Self::decode_alu_op(opcode),
            ),
            _ => unreachable!("unhandled opcode in 40-BF dispatcher: 0x{opcode:02X}"),
        }
        Ok(())
    }

    fn execute_opcode_c0_ff(&mut self, opcode: u8, opcode_pc: u16) -> Result<(), GbError> {
        let condition = Self::decode_condition((opcode >> 3) & 0x03);
        let stack_reg = Self::decode_stack_reg16((opcode >> 4) & 0x03);
        match opcode {
            0xC0 => self.exec_ret_condition_prefetch(condition),
            0xC1 => self.pop_stack_reg16_prefetch(stack_reg),
            0xC2 => self.exec_jp_condition_immediate(condition),
            0xC3 => self.exec_jp_immediate(),
            0xC4 => self.exec_call_condition_immediate(condition)?,
            0xC5 => self.push_stack_reg16_prefetch(stack_reg)?,
            0xC6 => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xC7 => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xC8 => self.exec_ret_condition_prefetch(condition),
            0xC9 => self.exec_ret_prefetch(),
            0xCA => self.exec_jp_condition_immediate(condition),
            0xCB => self.execute_cb_prefixed()?,
            0xCC => self.exec_call_condition_immediate(condition)?,
            0xCD => self.exec_call_immediate()?,
            0xCE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xCF => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xD0 => self.exec_ret_condition_prefetch(condition),
            0xD1 => self.pop_stack_reg16_prefetch(stack_reg),
            0xD2 => self.exec_jp_condition_immediate(condition),
            0xD3 => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xD4 => self.exec_call_condition_immediate(condition)?,
            0xD5 => self.push_stack_reg16_prefetch(stack_reg)?,
            0xD6 => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xD7 => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xD8 => self.exec_ret_condition_prefetch(condition),
            0xD9 => self.exec_reti(),
            0xDA => self.exec_jp_condition_immediate(condition),
            0xDB => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xDC => self.exec_call_condition_immediate(condition)?,
            0xDD => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xDE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xDF => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xE0 => {
                self.exec_load8(Out8::Addr(AddrMode8::ZeroPageImm), In8::Reg(Reg8Id::A));
            }
            0xE1 => self.pop_stack_reg16_prefetch(stack_reg),
            0xE2 => {
                self.exec_load8(Out8::Addr(AddrMode8::ZeroPageC), In8::Reg(Reg8Id::A));
            }
            0xE3 => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xE4 => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xE5 => self.push_stack_reg16_prefetch(stack_reg)?,
            0xE6 => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xE7 => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xE8 => {
                let offset = self.fetch8_timed_late() as i8;
                self.add_sp_signed_prefetch(offset);
            }
            0xE9 => self.prefetch_next_cycle(self.registers.hl()),
            0xEA => {
                self.exec_load8(Out8::Addr(AddrMode8::Direct), In8::Reg(Reg8Id::A));
            }
            0xEB => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xEC => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xED => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xEE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xEF => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xF0 => {
                self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::ZeroPageImm));
            }
            0xF1 => self.pop_stack_reg16_prefetch(stack_reg),
            0xF2 => {
                self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::ZeroPageC));
            }
            0xF3 => {
                self.ime = false;
                self.prefetch_no_interrupt_cycle(self.registers.pc);
            }
            0xF4 => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xF5 => self.push_stack_reg16_prefetch(stack_reg)?,
            0xF6 => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xF7 => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xF8 => {
                let offset = self.fetch8_timed_late() as i8;
                self.load_hl_sp_plus_signed_prefetch(offset);
            }
            0xF9 => self.load_sp_from_hl_prefetch(),
            0xFA => {
                self.exec_load8(Out8::Reg(Reg8Id::A), In8::Addr(AddrMode8::Direct));
            }
            0xFB => {
                self.prefetch_next_cycle(self.registers.pc);
                self.ime = true;
            }
            0xFC => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xFD => {
                return Err(GbError::IllegalOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
            0xFE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xFF => self.rst_to(Self::decode_rst_vector(opcode))?,
            _ => unreachable!("unhandled opcode in C0-FF dispatcher: 0x{opcode:02X}"),
        }
        Ok(())
    }

    fn execute_opcode(&mut self, opcode: u8, opcode_pc: u16) -> Result<(), GbError> {
        match opcode {
            0x00..=0x3F => self.execute_opcode_00_3f(opcode, opcode_pc),
            0x40..=0xBF => self.execute_opcode_40_bf(opcode, opcode_pc),
            0xC0..=0xFF => self.execute_opcode_c0_ff(opcode, opcode_pc),
        }
    }

    fn push16(&mut self, value: u16) -> Result<(), GbError> {
        let hi = (value >> 8) as u8;
        let lo = value as u8;
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write_cycle(self.registers.sp, hi);
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write_cycle(self.registers.sp, lo);
        Ok(())
    }

    fn pop16(&mut self) -> u16 {
        self.tick_timers(4);
        let lo = u16::from(self.read8_without_oam_bug(self.registers.sp));
        self.flush_pending_t34_interrupts();
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.registers.sp = self.registers.sp.wrapping_add(1);
        self.tick_timers(4);
        let hi = u16::from(self.read8_without_oam_bug(self.registers.sp));
        self.flush_pending_t34_interrupts();
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.registers.sp = self.registers.sp.wrapping_add(1);
        lo | (hi << 8)
    }

    fn request_interrupt(&mut self, mask: u8) {
        let value = self.io[(IF_REGISTER - IO_START) as usize] | mask | 0xE0;
        self.io[(IF_REGISTER - IO_START) as usize] = value;
    }

    fn request_interrupt_t34(&mut self, mask: u8) {
        self.pending_t34_interrupts |= mask;
    }

    fn flush_pending_t34_interrupts(&mut self) {
        if self.pending_t34_interrupts != 0 {
            let mask = self.pending_t34_interrupts;
            self.pending_t34_interrupts = 0;
            self.request_interrupt(mask);
        }
    }

    fn timer_bit_mask(&self) -> u16 {
        match self.io[(TIMER_TAC - IO_START) as usize] & 0x03 {
            0 => 1 << 7,
            1 => 1 << 1,
            2 => 1 << 3,
            _ => 1 << 5,
        }
    }

    fn timer_signal(&self) -> bool {
        let tac = self.io[(TIMER_TAC - IO_START) as usize];
        tac & 0x04 != 0 && (self.div_counter & self.timer_bit_mask()) != 0
    }

    fn increment_tima(&mut self) {
        let tima_index = (TIMER_TIMA - IO_START) as usize;
        let (next, overflow) = self.io[tima_index].overflowing_add(1);
        self.io[tima_index] = next;
        if overflow {
            self.io[tima_index] = 0;
            self.tima_reload_state = Some(TimaReloadState::OverflowDelay(1));
        }
    }

    fn step_timer_cycle(&mut self) {
        if let Some(state) = self.tima_reload_state {
            match state {
                TimaReloadState::OverflowDelay(remaining) => {
                    if remaining <= 1 {
                        let tma = self.io[(TIMER_TMA - IO_START) as usize];
                        self.io[(TIMER_TIMA - IO_START) as usize] = tma;
                        self.request_interrupt(0x04);
                        self.tima_reload_state = Some(TimaReloadState::ReloadWindow(1));
                    } else {
                        self.tima_reload_state =
                            Some(TimaReloadState::OverflowDelay(remaining - 1));
                    }
                }
                TimaReloadState::ReloadWindow(remaining) => {
                    if remaining <= 1 {
                        self.tima_reload_state = None;
                    } else {
                        self.tima_reload_state = Some(TimaReloadState::ReloadWindow(remaining - 1));
                    }
                }
            }
        }

        let old_signal = self.timer_signal();
        self.div_counter = self.div_counter.wrapping_add(1);
        self.io[(TIMER_DIV - IO_START) as usize] = (self.div_counter >> 6) as u8;
        let new_signal = self.timer_signal();
        if old_signal && !new_signal {
            self.increment_tima();
        }
    }

    fn switch_ppu_mode(&mut self, mode: PpuMode) {
        self.ppu_mode = mode;
        self.ppu_mode_cycles_remaining = self.ppu_mode_duration(mode);

        let stat = self.io[(STAT_REGISTER - IO_START) as usize];
        match mode {
            PpuMode::AccessOam => {
                if stat & 0x20 != 0 {
                    self.request_interrupt_t34(0x02);
                }
            }
            PpuMode::VBlank => {
                self.request_interrupt_t34(0x01);
                if stat & 0x10 != 0 {
                    self.request_interrupt_t34(0x02);
                }
                if stat & 0x20 != 0 {
                    self.request_interrupt_t34(0x02);
                }
            }
            PpuMode::HBlank | PpuMode::AccessVram => {}
        }
    }

    fn update_lyc_compare_interrupt(&mut self, old_match: bool) {
        let ly = self.io[(LY_REGISTER - IO_START) as usize];
        let lyc = self.io[(0xFF45 - IO_START) as usize];
        let new_match = ly == lyc;
        self.set_stat_lyc_flag(new_match);
        if new_match && !old_match && (self.io[(STAT_REGISTER - IO_START) as usize] & 0x40 != 0) {
            self.request_interrupt_t34(0x02);
        }
    }

    fn step_ppu_cycle(&mut self) {
        let lcdc = self.io[(LCDC_REGISTER - IO_START) as usize];
        if lcdc & 0x80 == 0 {
            return;
        }

        let old_match =
            self.io[(LY_REGISTER - IO_START) as usize] == self.io[(0xFF45 - IO_START) as usize];
        self.ppu_cycle_counter = (self.ppu_cycle_counter + 4) % 456;
        self.ppu_mode_cycles_remaining = self.ppu_mode_cycles_remaining.saturating_sub(1);

        if self.ppu_mode == PpuMode::AccessVram
            && self.ppu_mode_cycles_remaining == 1
            && (self.io[(STAT_REGISTER - IO_START) as usize] & 0x08 != 0)
        {
            self.request_interrupt_t34(0x02);
        }

        if self.ppu_mode_cycles_remaining > 0 {
            self.refresh_stat();
            return;
        }

        let ly_index = (LY_REGISTER - IO_START) as usize;
        match self.ppu_mode {
            PpuMode::AccessOam => self.switch_ppu_mode(PpuMode::AccessVram),
            PpuMode::AccessVram => self.switch_ppu_mode(PpuMode::HBlank),
            PpuMode::HBlank => {
                let next_ly = self.io[ly_index].wrapping_add(1);
                self.io[ly_index] = next_ly;
                if next_ly < 144 {
                    self.switch_ppu_mode(PpuMode::AccessOam);
                } else {
                    self.switch_ppu_mode(PpuMode::VBlank);
                }
                self.update_lyc_compare_interrupt(old_match);
            }
            PpuMode::VBlank => {
                let next_ly = self.io[ly_index].wrapping_add(1);
                if next_ly > 153 {
                    self.io[ly_index] = 0;
                    self.switch_ppu_mode(PpuMode::AccessOam);
                } else {
                    self.io[ly_index] = next_ly;
                    self.ppu_mode_cycles_remaining = PPU_VBLANK_LINE_CYCLES;
                }
                self.update_lyc_compare_interrupt(old_match);
            }
        }
        self.refresh_stat();
    }

    fn ppu_mode_duration(&self, mode: PpuMode) -> u16 {
        let scroll_adjust = match self.io[0x43] % 8 {
            5..=7 => 2,
            1..=4 => 1,
            _ => 0,
        };

        match mode {
            PpuMode::AccessOam => PPU_ACCESS_OAM_CYCLES,
            PpuMode::AccessVram => PPU_ACCESS_VRAM_CYCLES + scroll_adjust,
            PpuMode::HBlank => PPU_HBLANK_CYCLES - scroll_adjust,
            PpuMode::VBlank => PPU_VBLANK_LINE_CYCLES,
        }
    }

    fn pending_interrupts(&self) -> u8 {
        self.ie & self.io[(IF_REGISTER - IO_START) as usize] & 0x1F
    }

    fn highest_priority_interrupt(pending: u8) -> Option<(u8, u16)> {
        INTERRUPT_VECTORS
            .iter()
            .copied()
            .find(|(mask, _)| pending & mask != 0)
    }

    fn execute_interrupt_dispatch(&mut self) -> Result<bool, GbError> {
        let pending = self.pending_interrupts();
        let Some((selected_mask, vector)) = Self::highest_priority_interrupt(pending) else {
            self.set_exec_state(ExecState::Running);
            return Ok(false);
        };

        self.prefetched_pc = None;
        self.prefetched_opcode = None;
        self.ime = false;
        let pc = self.registers.pc;
        let hi = (pc >> 8) as u8;
        let lo = pc as u8;

        self.tick_mcycle();
        self.tick_mcycle();
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write_cycle(self.registers.sp, hi);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        let sampled_interrupts = self.write_cycle_intr(self.registers.sp, lo);

        let index = (IF_REGISTER - IO_START) as usize;
        let ack_mask = Self::highest_priority_interrupt(sampled_interrupts)
            .map(|(mask, _)| mask)
            .unwrap_or(selected_mask);
        self.io[index] = (self.io[index] & !ack_mask) | 0xE0;
        self.registers.pc = vector;
        let opcode_pc = self.registers.pc;
        let opcode = self.fetch8_cycle();
        self.prefetched_pc = Some(opcode_pc);
        self.prefetched_opcode = Some(opcode);
        self.set_exec_state(ExecState::Running);
        Ok(true)
    }

    fn tick_timers(&mut self, cycles: u16) {
        self.cartridge.tick(cycles);
        for _ in 0..cycles {
            self.cycle_counter += 1;
            if self.cycle_counter % 4 == 0 {
                self.step_dma_mcycle();
                self.step_ppu_cycle();
                self.step_timer_cycle();
            }
        }
    }

    fn execute_next_instruction(&mut self) -> Result<RunResult, GbError> {
        self.pending_watchpoint = None;

        loop {
            match self.exec_state {
                ExecState::InterruptDispatch => {
                    if self.execute_interrupt_dispatch()? {
                        return Ok(RunResult {
                            stop_reason: StopReason::StepComplete,
                        });
                    }
                }
                ExecState::Halt => {
                    if self.pending_interrupts() == 0 {
                        self.tick_mcycle();
                        return Ok(RunResult {
                            stop_reason: StopReason::Halted,
                        });
                    }

                    self.set_exec_state(ExecState::Running);
                    self.prefetch_next_cycle(self.registers.pc);
                    if matches!(self.exec_state, ExecState::InterruptDispatch) {
                        continue;
                    }

                    return Ok(RunResult {
                        stop_reason: StopReason::StepComplete,
                    });
                }
                ExecState::Stop => {
                    if self.pending_interrupts() == 0 && self.current_joypad_bits() == 0x0F {
                        self.tick_mcycle();
                        return Ok(RunResult {
                            stop_reason: StopReason::Halted,
                        });
                    }

                    self.set_exec_state(ExecState::Running);
                    self.prefetch_next_cycle(self.registers.pc);
                    if matches!(self.exec_state, ExecState::InterruptDispatch) {
                        continue;
                    }

                    return Ok(RunResult {
                        stop_reason: StopReason::StepComplete,
                    });
                }
                ExecState::Running => {}
            }
            break;
        }

        let pc = self.logical_pc();
        if self.has_pc_breakpoint(pc) {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }
        if self.has_opcode_breakpoint(self.logical_opcode()) {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }

        let (opcode_pc, opcode) = self.consume_opcode();

        self.execute_opcode(opcode, opcode_pc)?;

        self.instruction_counter += 1;

        let stop_reason = if matches!(self.exec_state, ExecState::Halt) {
            StopReason::Halted
        } else if self.pending_watchpoint.is_some() {
            StopReason::WatchpointHit
        } else if self.has_pc_breakpoint(self.logical_pc()) {
            StopReason::BreakpointHit
        } else {
            StopReason::StepComplete
        };

        self.push_trace(TraceEntry::from_state(
            self.instruction_counter,
            opcode_pc,
            opcode,
            self.registers,
            stop_reason,
        ));

        Ok(RunResult { stop_reason })
    }

    fn system_memory_slice(&self, address: u16, len: usize) -> Option<Vec<u8>> {
        let end = address.checked_add(u16::try_from(len).ok()?.saturating_sub(1))?;
        let mut bytes = Vec::with_capacity(len);
        for current in address..=end {
            bytes.push(self.peek8(current));
        }
        Some(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GbModel {
    Dmg0,
    Dmg,
    Mgb,
    Sgb,
    Sgb2,
}

impl GbModel {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dmg0" => Some(Self::Dmg0),
            "dmg" => Some(Self::Dmg),
            "mgb" => Some(Self::Mgb),
            "sgb" => Some(Self::Sgb),
            "sgb2" => Some(Self::Sgb2),
            _ => None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::Dmg0 => "dmg0",
            Self::Dmg => "dmg",
            Self::Mgb => "mgb",
            Self::Sgb => "sgb",
            Self::Sgb2 => "sgb2",
        }
    }
}
