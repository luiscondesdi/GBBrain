//! Minimal Game Boy DMG machine skeleton with usable debug plumbing.

use std::{collections::VecDeque, error::Error, fmt};

use gbbrain_core::{
    AddressSpace, Breakpoint, CpuRegisters, FrameBuffer, Machine, MachineControl, MachineSnapshot,
    MemoryRegion, RenderTarget, RunResult, StopReason,
};
use serde::{Deserialize, Serialize};

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
    ReadWrite,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum TimaReloadState {
    OverflowDelay(u8),
    ReloadWindow(u8),
}

#[derive(Debug, Clone)]
pub enum GbError {
    EmptyRom,
    UnsupportedOpcode { opcode: u8, pc: u16 },
    StackOverflow(u16),
}

impl fmt::Display for GbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRom => f.write_str("ROM is empty"),
            Self::UnsupportedOpcode { opcode, pc } => {
                write!(f, "unsupported opcode 0x{opcode:02x} at PC 0x{pc:04x}")
            }
            Self::StackOverflow(address) => {
                write!(f, "stack access failed at address 0x{address:04x}")
            }
        }
    }
}

impl Error for GbError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[derive(Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
enum WatchpointKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
struct WatchpointHit {
    kind: WatchpointKind,
    address: u16,
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
    rom: Vec<u8>,
    eram: [u8; 0x2000],
    vram: [u8; 0x2000],
    wram: [u8; 0x2000],
    oam: [u8; 0xA0],
    io: [u8; 0x80],
    hram: [u8; 0x7F],
    ie: u8,
    registers: Registers,
    breakpoints: Vec<Breakpoint>,
    ime: bool,
    ime_enable_delay: u8,
    halted: bool,
    halt_bug: bool,
    instruction_counter: u64,
    cycle_counter: u64,
    div_counter: u16,
    tima_reload_state: Option<TimaReloadState>,
    ppu_cycle_counter: u16,
    dma_source: u16,
    dma_delay_cycles: u16,
    dma_pending_source: Option<u16>,
    dma_pending_delay_cycles: u16,
    dma_cycles_remaining: u16,
    dma_byte_cycles: u8,
    dma_next_byte: u16,
    pending_watchpoint: Option<WatchpointHit>,
    trace: VecDeque<TraceEntry>,
    serial_output: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct DebugState {
    pub cycle_counter: u64,
    pub div_counter: u16,
    pub ppu_cycle_counter: u16,
    pub ime: bool,
    pub ie: u8,
    pub if_reg: u8,
    pub lcdc: u8,
    pub stat: u8,
    pub ly: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotState {
    rom: Vec<u8>,
    eram: Vec<u8>,
    vram: Vec<u8>,
    wram: Vec<u8>,
    oam: Vec<u8>,
    io: Vec<u8>,
    hram: Vec<u8>,
    ie: u8,
    registers: Registers,
    breakpoints: Vec<SnapshotBreakpoint>,
    ime: bool,
    ime_enable_delay: u8,
    halted: bool,
    halt_bug: bool,
    instruction_counter: u64,
    cycle_counter: u64,
    div_counter: u16,
    tima_reload_state: Option<TimaReloadState>,
    ppu_cycle_counter: u16,
    dma_source: u16,
    dma_delay_cycles: u16,
    dma_pending_source: Option<u16>,
    dma_pending_delay_cycles: u16,
    dma_cycles_remaining: u16,
    dma_byte_cycles: u8,
    dma_next_byte: u16,
    pending_watchpoint: Option<WatchpointHit>,
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

impl GbMachine {
    pub fn new(rom: Vec<u8>) -> Result<Self, GbError> {
        if rom.is_empty() {
            return Err(GbError::EmptyRom);
        }

        let mut machine = Self {
            rom,
            eram: [0; 0x2000],
            vram: [0; 0x2000],
            wram: [0; 0x2000],
            oam: [0; 0xA0],
            io: [0; 0x80],
            hram: [0; 0x7F],
            ie: 0,
            registers: Registers {
                sp: 0xFFFE,
                pc: 0x0100,
                ..Registers::default()
            },
            breakpoints: Vec::new(),
            ime: false,
            ime_enable_delay: 0,
            halted: false,
            halt_bug: false,
            instruction_counter: 0,
            cycle_counter: 0,
            div_counter: 0,
            tima_reload_state: None,
            ppu_cycle_counter: 0,
            dma_source: 0,
            dma_delay_cycles: 0,
            dma_pending_source: None,
            dma_pending_delay_cycles: 0,
            dma_cycles_remaining: 0,
            dma_byte_cycles: 0,
            dma_next_byte: 0,
            pending_watchpoint: None,
            trace: VecDeque::with_capacity(TRACE_CAPACITY),
            serial_output: Vec::new(),
        };
        machine.reset_state();
        Ok(machine)
    }

    fn reset_state(&mut self) {
        self.eram.fill(0);
        self.vram.fill(0);
        self.wram.fill(0);
        self.oam.fill(0);
        self.io.fill(0xFF);
        self.hram.fill(0);
        self.ie = 0;
        self.registers = Registers {
            a: 0x01,
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc: 0x0100,
        };
        self.breakpoints.clear();
        self.ime = false;
        self.ime_enable_delay = 0;
        self.halted = false;
        self.halt_bug = false;
        self.instruction_counter = 0;
        self.cycle_counter = 0;
        self.div_counter = 0xABD0;
        self.tima_reload_state = None;
        self.ppu_cycle_counter = 0;
        self.dma_source = 0;
        self.dma_delay_cycles = 0;
        self.dma_pending_source = None;
        self.dma_pending_delay_cycles = 0;
        self.dma_cycles_remaining = 0;
        self.dma_byte_cycles = 0;
        self.dma_next_byte = 0;
        self.pending_watchpoint = None;
        self.trace.clear();
        self.serial_output.clear();
        self.io[0x00] = 0xCF; // P1
        self.io[(SERIAL_SB - IO_START) as usize] = 0x00;
        self.io[(SERIAL_SC - IO_START) as usize] = 0x7E;
        self.io[(TIMER_DIV - IO_START) as usize] = 0xAB;
        self.io[(TIMER_TIMA - IO_START) as usize] = 0x00;
        self.io[(TIMER_TMA - IO_START) as usize] = 0x00;
        self.io[(TIMER_TAC - IO_START) as usize] = 0xF8;
        self.io[(IF_REGISTER - IO_START) as usize] = 0xE1;
        self.io[0x10] = 0x80; // NR10
        self.io[0x11] = 0xBF; // NR11
        self.io[0x12] = 0xF3; // NR12
        self.io[0x13] = 0xFF; // NR13
        self.io[0x14] = 0xBF; // NR14
        self.io[0x16] = 0x3F; // NR21
        self.io[0x17] = 0x00; // NR22
        self.io[0x18] = 0xFF; // NR23
        self.io[0x19] = 0xBF; // NR24
        self.io[0x1A] = 0x7F; // NR30
        self.io[0x1B] = 0xFF; // NR31
        self.io[0x1C] = 0x9F; // NR32
        self.io[0x1D] = 0xFF; // NR33
        self.io[0x1E] = 0xBF; // NR34
        self.io[0x20] = 0xFF; // NR41
        self.io[0x21] = 0x00; // NR42
        self.io[0x22] = 0x00; // NR43
        self.io[0x23] = 0xBF; // NR44
        self.io[0x24] = 0x77; // NR50
        self.io[0x25] = 0xF3; // NR51
        self.io[0x26] = 0xF1; // NR52
        self.io[(LCDC_REGISTER - IO_START) as usize] = 0x91;
        self.io[(STAT_REGISTER - IO_START) as usize] = 0x80;
        self.io[0x42] = 0x00; // SCY
        self.io[0x43] = 0x00; // SCX
        self.io[(LY_REGISTER - IO_START) as usize] = 0x00;
        self.io[0x45] = 0x00; // LYC
        self.io[(DMA_REGISTER - IO_START) as usize] = 0xFF;
        self.io[0x47] = 0xFC; // BGP
        self.io[0x48] = 0xFF; // OBP0
        self.io[0x49] = 0xFF; // OBP1
        self.io[0x4A] = 0x00; // WY
        self.io[0x4B] = 0x00; // WX
        self.refresh_stat();
    }

    pub fn trace_entries(&self) -> Vec<TraceEntry> {
        self.trace.iter().copied().collect()
    }

    pub fn clear_trace(&mut self) {
        self.trace.clear();
    }

    pub fn serial_output(&self) -> &[u8] {
        &self.serial_output
    }

    pub fn clear_serial_output(&mut self) {
        self.serial_output.clear();
    }

    pub fn last_watchpoint(&self) -> Option<(&'static str, u16)> {
        self.pending_watchpoint.map(|hit| {
            let kind = match hit.kind {
                WatchpointKind::Read => "read",
                WatchpointKind::Write => "write",
            };
            (kind, hit.address)
        })
    }

    pub fn debug_state(&self) -> DebugState {
        DebugState {
            cycle_counter: self.cycle_counter,
            div_counter: self.div_counter,
            ppu_cycle_counter: self.ppu_cycle_counter,
            ime: self.ime,
            ie: self.ie,
            if_reg: self.io[(IF_REGISTER - IO_START) as usize],
            lcdc: self.io[(LCDC_REGISTER - IO_START) as usize],
            stat: self.io[(STAT_REGISTER - IO_START) as usize],
            ly: self.io[(LY_REGISTER - IO_START) as usize],
        }
    }

    pub fn save_state(&self) -> Result<Vec<u8>, GbError> {
        let state = SnapshotState {
            rom: self.rom.clone(),
            eram: self.eram.to_vec(),
            vram: self.vram.to_vec(),
            wram: self.wram.to_vec(),
            oam: self.oam.to_vec(),
            io: self.io.to_vec(),
            hram: self.hram.to_vec(),
            ie: self.ie,
            registers: self.registers,
            breakpoints: self
                .breakpoints
                .iter()
                .copied()
                .map(SnapshotBreakpoint::from)
                .collect(),
            ime: self.ime,
            ime_enable_delay: self.ime_enable_delay,
            halted: self.halted,
            halt_bug: self.halt_bug,
            instruction_counter: self.instruction_counter,
            cycle_counter: self.cycle_counter,
            div_counter: self.div_counter,
            tima_reload_state: self.tima_reload_state,
            ppu_cycle_counter: self.ppu_cycle_counter,
            dma_source: self.dma_source,
            dma_delay_cycles: self.dma_delay_cycles,
            dma_pending_source: self.dma_pending_source,
            dma_pending_delay_cycles: self.dma_pending_delay_cycles,
            dma_cycles_remaining: self.dma_cycles_remaining,
            dma_byte_cycles: self.dma_byte_cycles,
            dma_next_byte: self.dma_next_byte,
            pending_watchpoint: self.pending_watchpoint,
            trace: self.trace.iter().copied().map(SnapshotTraceEntry::from).collect(),
            serial_output: self.serial_output.clone(),
        };

        serde_json::to_vec(&state).map_err(|_| GbError::StackOverflow(0))
    }

    pub fn load_state(bytes: &[u8]) -> Result<Self, GbError> {
        let state: SnapshotState =
            serde_json::from_slice(bytes).map_err(|_| GbError::StackOverflow(0))?;
        if state.rom.is_empty() {
            return Err(GbError::EmptyRom);
        }

        let mut machine = Self::new(state.rom)?;
        machine.eram.copy_from_slice(&state.eram[..0x2000]);
        machine.vram.copy_from_slice(&state.vram[..0x2000]);
        machine.wram.copy_from_slice(&state.wram[..0x2000]);
        machine.oam.copy_from_slice(&state.oam[..0xA0]);
        machine.io.copy_from_slice(&state.io[..0x80]);
        machine.hram.copy_from_slice(&state.hram[..0x7F]);
        machine.ie = state.ie;
        machine.registers = state.registers;
        machine.breakpoints = state.breakpoints.into_iter().map(Breakpoint::from).collect();
        machine.ime = state.ime;
        machine.ime_enable_delay = state.ime_enable_delay;
        machine.halted = state.halted;
        machine.halt_bug = state.halt_bug;
        machine.instruction_counter = state.instruction_counter;
        machine.cycle_counter = state.cycle_counter;
        machine.div_counter = state.div_counter;
        machine.tima_reload_state = state.tima_reload_state;
        machine.ppu_cycle_counter = state.ppu_cycle_counter;
        machine.dma_source = state.dma_source;
        machine.dma_delay_cycles = state.dma_delay_cycles;
        machine.dma_pending_source = state.dma_pending_source;
        machine.dma_pending_delay_cycles = state.dma_pending_delay_cycles;
        machine.dma_cycles_remaining = state.dma_cycles_remaining;
        machine.dma_byte_cycles = state.dma_byte_cycles;
        machine.dma_next_byte = state.dma_next_byte;
        machine.pending_watchpoint = state.pending_watchpoint;
        machine.trace = state.trace.into_iter().map(TraceEntry::from).collect();
        machine.serial_output = state.serial_output;
        machine.refresh_stat();
        Ok(machine)
    }

    pub fn disassemble_range(&self, start: u16, count: usize) -> Vec<DisassembledInstruction> {
        let mut out = Vec::with_capacity(count);
        let mut pc = start;
        for _ in 0..count {
            let inst = self.disassemble_one(pc);
            pc = pc.wrapping_add(u16::from(inst.len.max(1)));
            out.push(inst);
        }
        out
    }

    fn disassemble_one(&self, address: u16) -> DisassembledInstruction {
        let opcode = self.peek8(address);
        let b1 = self.peek8(address.wrapping_add(1));
        let b2 = self.peek8(address.wrapping_add(2));
        let word = u16::from_le_bytes([b1, b2]);
        let (len, text) = match opcode {
            0x00 => (1, "NOP".to_string()),
            0x01 => (3, format!("LD BC,${word:04X}")),
            0x03 => (1, "INC BC".to_string()),
            0x04 => (1, "INC B".to_string()),
            0x05 => (1, "DEC B".to_string()),
            0x06 => (2, format!("LD B,${b1:02X}")),
            0x0D => (1, "DEC C".to_string()),
            0x0E => (2, format!("LD C,${b1:02X}")),
            0x11 => (3, format!("LD DE,${word:04X}")),
            0x13 => (1, "INC DE".to_string()),
            0x18 => (2, format!("JR {:+}", b1 as i8)),
            0x20 => (2, format!("JR NZ,{:+}", b1 as i8)),
            0x21 => (3, format!("LD HL,${word:04X}")),
            0x22 => (1, "LD (HL+),A".to_string()),
            0x23 => (1, "INC HL".to_string()),
            0x28 => (2, format!("JR Z,{:+}", b1 as i8)),
            0x2A => (1, "LD A,(HL+)".to_string()),
            0x31 => (3, format!("LD SP,${word:04X}")),
            0x32 => (1, "LD (HL-),A".to_string()),
            0x3A => (1, "LD A,(HL-)".to_string()),
            0x3C => (1, "INC A".to_string()),
            0x3D => (1, "DEC A".to_string()),
            0x3E => (2, format!("LD A,${b1:02X}")),
            0x76 => (1, "HALT".to_string()),
            0x77 => (1, "LD (HL),A".to_string()),
            0x78..=0x7F => (1, format!("LD A,{}", r8_name(opcode & 0x07))),
            0xA8..=0xAF => (1, format!("XOR {}", r8_name(opcode & 0x07))),
            0xB0..=0xB7 => (1, format!("OR {}", r8_name(opcode & 0x07))),
            0xC1 => (1, "POP BC".to_string()),
            0xC3 => (3, format!("JP ${word:04X}")),
            0xC5 => (1, "PUSH BC".to_string()),
            0xC9 => (1, "RET".to_string()),
            0xCD => (3, format!("CALL ${word:04X}")),
            0xD1 => (1, "POP DE".to_string()),
            0xD5 => (1, "PUSH DE".to_string()),
            0xE0 => (2, format!("LDH ($FF{b1:02X}),A")),
            0xE1 => (1, "POP HL".to_string()),
            0xE5 => (1, "PUSH HL".to_string()),
            0xEA => (3, format!("LD (${word:04X}),A")),
            0xF0 => (2, format!("LDH A,($FF{b1:02X})")),
            0xF1 => (1, "POP AF".to_string()),
            0xF3 => (1, "DI".to_string()),
            0xF5 => (1, "PUSH AF".to_string()),
            0xFB => (1, "EI".to_string()),
            0xFE => (2, format!("CP ${b1:02X}")),
            0xCB => (2, format!("CB {}", cb_name(b1))),
            _ => (1, format!("DB ${opcode:02X}")),
        };
        let bytes = (0..len)
            .map(|offset| self.peek8(address.wrapping_add(u16::from(offset))))
            .collect();
        DisassembledInstruction {
            address,
            bytes,
            text,
            len,
        }
    }

    fn push_trace(&mut self, entry: TraceEntry) {
        if self.trace.len() == TRACE_CAPACITY {
            self.trace.pop_front();
        }
        self.trace.push_back(entry);
    }

    fn has_pc_breakpoint(&self, pc: u16) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| matches!(bp, Breakpoint::ProgramCounter(value) if *value == u32::from(pc)))
    }

    fn has_opcode_breakpoint(&self, opcode: u8) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| matches!(bp, Breakpoint::Opcode(value) if *value == opcode))
    }

    fn matching_watchpoint(&self, kind: WatchpointKind, address: u16) -> bool {
        self.breakpoints.iter().any(|bp| match (bp, kind) {
            (Breakpoint::MemoryRead(value), WatchpointKind::Read) => *value == u32::from(address),
            (Breakpoint::MemoryWrite(value), WatchpointKind::Write) => *value == u32::from(address),
            _ => false,
        })
    }

    fn record_watchpoint(&mut self, kind: WatchpointKind, address: u16) {
        if self.matching_watchpoint(kind, address) {
            self.pending_watchpoint = Some(WatchpointHit { kind, address });
        }
    }

    fn cpu_bus(address: u16) -> Bus {
        match address {
            0x0000..=ROM_BANK_N_END | 0xA000..=0xBFFF | WRAM_START..=WRAM_END | ECHO_START..=ECHO_END => Bus::External,
            VRAM_START..=VRAM_END => Bus::ExternalVideo,
            OAM_START..=OAM_END => Bus::InternalVideo,
            _ => Bus::Internal,
        }
    }

    fn dma_source_bus(&self) -> Option<Bus> {
        if !self.dma_active() {
            return None;
        }
        let high = (self.dma_source >> 8) as u8;
        Some(match high {
            0x80..=0x9F => Bus::ExternalVideo,
            _ => Bus::External,
        })
    }

    fn dma_blocks_cpu_access(&self, address: u16) -> bool {
        let cpu_bus = Self::cpu_bus(address);
        if cpu_bus == Bus::InternalVideo {
            return self.dma_active();
        }
        matches!(self.dma_source_bus(), Some(source_bus) if source_bus == cpu_bus)
    }

    fn lcd_enabled(&self) -> bool {
        self.io[(LCDC_REGISTER - IO_START) as usize] & 0x80 != 0
    }

    fn current_ly(&self) -> u8 {
        self.io[(LY_REGISTER - IO_START) as usize]
    }

    fn ppu_mode(&self) -> u8 {
        if !self.lcd_enabled() {
            return 0;
        }
        if self.current_ly() >= 144 {
            return 1;
        }
        if self.ppu_cycle_counter < 80 {
            2
        } else if self.ppu_cycle_counter < 252 {
            3
        } else {
            0
        }
    }

    fn refresh_stat(&mut self) {
        let stat_index = (STAT_REGISTER - IO_START) as usize;
        let lyc_index = (0xFF45 - IO_START) as usize;
        let ly_equals_lyc = self.io[(LY_REGISTER - IO_START) as usize] == self.io[lyc_index];
        let mut stat = self.io[stat_index] & 0xF8;
        if ly_equals_lyc {
            stat |= 0x04;
        }
        stat |= self.ppu_mode() & 0x03;
        self.io[stat_index] = stat;
    }

    fn oam_bug_row(&self) -> Option<usize> {
        if self.ppu_mode() == 2 {
            Some(usize::from((self.ppu_cycle_counter / 4).min(19)))
        } else {
            None
        }
    }

    fn oam_bug_applies(address: u16) -> bool {
        (0xFE00..=0xFEFF).contains(&address)
    }

    fn oam_word(&self, row: usize, word: usize) -> u16 {
        let base = row * 8 + word * 2;
        u16::from_le_bytes([self.oam[base], self.oam[base + 1]])
    }

    fn set_oam_word(&mut self, row: usize, word: usize, value: u16) {
        let base = row * 8 + word * 2;
        let [lo, hi] = value.to_le_bytes();
        self.oam[base] = lo;
        self.oam[base + 1] = hi;
    }

    fn apply_oam_row_corruption(&mut self, row: usize, kind: OamCorruptionKind) {
        if row == 0 || row >= 20 {
            return;
        }

        match kind {
            OamCorruptionKind::Write => {
                let a = self.oam_word(row, 0);
                let b = self.oam_word(row - 1, 0);
                let c = self.oam_word(row - 1, 2);
                self.set_oam_word(row, 0, ((a ^ c) & (b ^ c)) ^ c);
                for word in 1..4 {
                    self.set_oam_word(row, word, self.oam_word(row - 1, word));
                }
            }
            OamCorruptionKind::Read => {
                let a = self.oam_word(row, 0);
                let b = self.oam_word(row - 1, 0);
                let c = self.oam_word(row - 1, 2);
                self.set_oam_word(row, 0, b | (a & c));
                for word in 1..4 {
                    self.set_oam_word(row, word, self.oam_word(row - 1, word));
                }
            }
            OamCorruptionKind::ReadWrite => {
                if row >= 4 && row < 19 {
                    let a = self.oam_word(row - 2, 0);
                    let b = self.oam_word(row - 1, 0);
                    let c = self.oam_word(row, 0);
                    let d = self.oam_word(row - 1, 2);
                    let corrupted_prev = (b & (a | c | d)) | (a & c & d);
                    self.set_oam_word(row - 1, 0, corrupted_prev);
                    for word in 1..4 {
                        let prev = self.oam_word(row - 1, word);
                        self.set_oam_word(row, word, prev);
                        self.set_oam_word(row - 2, word, prev);
                    }
                    self.set_oam_word(row, 0, corrupted_prev);
                    self.set_oam_word(row - 2, 0, corrupted_prev);
                }
                self.apply_oam_row_corruption(row, OamCorruptionKind::Read);
            }
        }
    }

    fn maybe_trigger_oam_bug(&mut self, address: u16, kind: OamCorruptionKind) {
        if !Self::oam_bug_applies(address) {
            return;
        }
        if let Some(row) = self.oam_bug_row() {
            self.apply_oam_row_corruption(row, kind);
        }
    }

    fn read8_with_kind(&mut self, address: u16, kind: OamCorruptionKind) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
            self.maybe_trigger_oam_bug(address, kind);
            return 0xFF;
        }
        if self.ppu_mode() == 3 && matches!(address, VRAM_START..=VRAM_END | OAM_START..=OAM_END) {
            return 0xFF;
        }
        if self.dma_blocks_cpu_access(address) {
            return 0xFF;
        }
        self.peek8(address)
    }

    fn read8(&mut self, address: u16) -> u8 {
        self.read8_with_kind(address, OamCorruptionKind::Read)
    }

    fn peek8(&self, address: u16) -> u8 {
        if self.dma_blocks_cpu_access(address) {
            return 0xFF;
        }
        match address {
            0x0000..=ROM_BANK_0_END | 0x4000..=ROM_BANK_N_END => {
                self.rom.get(address as usize).copied().unwrap_or(0xFF)
            }
            0xA000..=0xBFFF => self.eram[(address - 0xA000) as usize],
            VRAM_START..=VRAM_END => self.vram[(address - VRAM_START) as usize],
            WRAM_START..=WRAM_END => self.wram[(address - WRAM_START) as usize],
            ECHO_START..=ECHO_END => self.wram[(address - ECHO_START) as usize],
            OAM_START..=OAM_END => self.oam[(address - OAM_START) as usize],
            IO_START..=IO_END => self.io[(address - IO_START) as usize],
            HRAM_START..=HRAM_END => self.hram[(address - HRAM_START) as usize],
            IE_REGISTER => self.ie,
            _ => 0xFF,
        }
    }

    fn write8(&mut self, address: u16, value: u8) {
        self.record_watchpoint(WatchpointKind::Write, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
            self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
            return;
        }
        if self.ppu_mode() == 3 && matches!(address, VRAM_START..=VRAM_END | OAM_START..=OAM_END) {
            return;
        }
        if self.dma_blocks_cpu_access(address) {
            return;
        }
        match address {
            0xA000..=0xBFFF => self.eram[(address - 0xA000) as usize] = value,
            VRAM_START..=VRAM_END => self.vram[(address - VRAM_START) as usize] = value,
            WRAM_START..=WRAM_END => self.wram[(address - WRAM_START) as usize] = value,
            ECHO_START..=ECHO_END => self.wram[(address - ECHO_START) as usize] = value,
            OAM_START..=OAM_END => {
                if !self.dma_active() {
                    self.oam[(address - OAM_START) as usize] = value;
                }
            }
            IO_START..=IO_END => self.write_io(address, value),
            HRAM_START..=HRAM_END => self.hram[(address - HRAM_START) as usize] = value,
            IE_REGISTER => self.ie = value,
            _ => {}
        }
    }

    fn dma_active(&self) -> bool {
        self.dma_delay_cycles == 0 && self.dma_cycles_remaining > 0
    }

    fn begin_dma(&mut self, source: u16, delay_cycles: u16) {
        self.dma_source = source;
        self.dma_delay_cycles = delay_cycles;
        self.dma_cycles_remaining = 640;
        self.dma_byte_cycles = 0;
        self.dma_next_byte = 0;
    }

    fn dma_source_byte(&self, address: u16) -> u8 {
        match address {
            0x0000..=ROM_BANK_0_END | 0x4000..=ROM_BANK_N_END => {
                self.rom.get(address as usize).copied().unwrap_or(0xFF)
            }
            0x8000..=0x9FFF => self.vram[(address - 0x8000) as usize],
            0xA000..=0xBFFF => self.eram[(address - 0xA000) as usize],
            0xC000..=0xDFFF => self.wram[(address - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(address - 0xE000) as usize],
            // OAM DMA does not use the CPU's usual FE/FF decoding; these ranges source from the
            // external bus instead, which makes them mirror the WRAM echo area on DMG.
            0xFE00..=0xFFFF => {
                let external_address = address.wrapping_sub(0x2000);
                self.dma_source_byte(external_address)
            }
        }
    }

    fn write_io(&mut self, address: u16, value: u8) {
        match address {
            LCDC_REGISTER => {
                let was_enabled = self.lcd_enabled();
                self.io[(address - IO_START) as usize] = value;
                let now_enabled = value & 0x80 != 0;
                if !was_enabled && now_enabled {
                    self.ppu_cycle_counter = 4;
                    self.io[(LY_REGISTER - IO_START) as usize] = 0;
                } else if was_enabled && !now_enabled {
                    self.ppu_cycle_counter = 0;
                    self.io[(LY_REGISTER - IO_START) as usize] = 0;
                }
                self.refresh_stat();
            }
            TIMER_DIV => {
                let old_signal = self.timer_signal();
                self.div_counter = 0;
                self.io[(address - IO_START) as usize] = 0;
                if old_signal && !self.timer_signal() {
                    self.increment_tima();
                }
            }
            TIMER_TIMA => {
                match self.tima_reload_state {
                    Some(TimaReloadState::OverflowDelay(_)) => {
                        self.tima_reload_state = None;
                    }
                    Some(TimaReloadState::ReloadWindow(_)) => {
                        return;
                    }
                    None => {}
                }
                self.io[(address - IO_START) as usize] = value;
            }
            TIMER_TMA => {
                self.io[(address - IO_START) as usize] = value;
                if matches!(self.tima_reload_state, Some(TimaReloadState::ReloadWindow(_))) {
                    self.io[(TIMER_TIMA - IO_START) as usize] = value;
                }
            }
            SERIAL_SB => {
                self.io[(address - IO_START) as usize] = value;
            }
            DMA_REGISTER => {
                self.io[(address - IO_START) as usize] = value;
                let source = u16::from(value) << 8;
                if self.dma_active() {
                    self.dma_pending_source = Some(source);
                    self.dma_pending_delay_cycles = 8;
                } else {
                    self.dma_pending_source = None;
                    self.dma_pending_delay_cycles = 0;
                    self.begin_dma(source, 8);
                }
            }
            TIMER_TAC => {
                let old_signal = self.timer_signal();
                self.io[(address - IO_START) as usize] = value;
                if old_signal && !self.timer_signal() {
                    self.increment_tima();
                }
            }
            SERIAL_SC => {
                self.io[(address - IO_START) as usize] = value;
                if value == 0x81 {
                    let byte = self.io[(SERIAL_SB - IO_START) as usize];
                    self.serial_output.push(byte);
                    self.io[(SERIAL_SC - IO_START) as usize] = 0x01;
                    self.request_interrupt(0x08);
                }
            }
            IF_REGISTER => {
                self.io[(address - IO_START) as usize] = value | 0xE0;
            }
            _ => {
                self.io[(address - IO_START) as usize] = value;
            }
        }
    }

    fn fetch8(&mut self) -> u8 {
        let pc = self.registers.pc;
        let value = if Self::oam_bug_applies(pc) && self.ppu_mode() == 2 {
            self.read8_with_kind(pc, OamCorruptionKind::ReadWrite)
        } else {
            self.read8(pc)
        };
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.registers.pc = self.registers.pc.wrapping_add(1);
        }
        value
    }

    fn fetch16(&mut self) -> u16 {
        let lo = u16::from(self.fetch8());
        let hi = u16::from(self.fetch8());
        lo | (hi << 8)
    }

    fn fetch8_timed_late(&mut self) -> u8 {
        self.tick_timers(4);
        let value = self.read8(self.registers.pc);
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.registers.pc = self.registers.pc.wrapping_add(1);
        }
        value
    }

    fn fetch16_timed_late(&mut self) -> u16 {
        let lo = u16::from(self.fetch8_timed_late());
        let hi = u16::from(self.fetch8_timed_late());
        lo | (hi << 8)
    }

    fn read_r8(&mut self, index: u8) -> u8 {
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
        self.tick_timers(4);
        self.read8(self.registers.hl())
    }

    fn write_hl_timed_late(&mut self, value: u8) {
        self.tick_timers(4);
        self.write8(self.registers.hl(), value);
    }

    fn write_r8(&mut self, index: u8, value: u8) {
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
        self.set_flag(0x10, (u16::from(a) + u16::from(value) + u16::from(carry)) > 0xFF);
    }

    fn flag_z(&self) -> bool {
        self.registers.f & 0x80 != 0
    }

    fn flag_c(&self) -> bool {
        self.registers.f & 0x10 != 0
    }

    fn condition_true(&self, code: u8) -> bool {
        match code {
            0 => !self.flag_z(),
            1 => self.flag_z(),
            2 => !self.flag_c(),
            3 => self.flag_c(),
            _ => false,
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

    fn jump_relative(&mut self) {
        let offset = self.fetch8() as i8;
        self.registers.pc = self.registers.pc.wrapping_add_signed(i16::from(offset));
    }

    fn execute_cb_prefixed(&mut self) -> Result<u16, GbError> {
        let opcode = self.fetch8();
        match opcode {
            0x00..=0x07 => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry = value & 0x80 != 0;
                let result = value.rotate_left(1);
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x08..=0x0F => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry = value & 0x01 != 0;
                let result = value.rotate_right(1);
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x10..=0x17 => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry_in = u8::from(self.flag_c());
                let carry_out = value & 0x80 != 0;
                let result = (value << 1) | carry_in;
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry_out);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x18..=0x1F => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry_in = if self.flag_c() { 0x80 } else { 0 };
                let carry_out = value & 0x01 != 0;
                let result = (value >> 1) | carry_in;
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry_out);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x20..=0x27 => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry = value & 0x80 != 0;
                let result = value << 1;
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x28..=0x2F => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry = value & 0x01 != 0;
                let result = (value >> 1) | (value & 0x80);
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x30..=0x37 => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let result = value.rotate_left(4);
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, false);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x38..=0x3F => {
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                let carry = value & 0x01 != 0;
                let result = value >> 1;
                if target == 6 {
                    self.write_hl_timed_late(result);
                } else {
                    self.write_r8(target, result);
                }
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x40..=0x7F => {
                let bit = (opcode - 0x40) / 8;
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                };
                self.set_flag(0x80, value & (1 << bit) == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, true);
                Ok(if target == 6 { 8 } else { 8 })
            }
            0x80..=0xBF => {
                let bit = (opcode - 0x80) / 8;
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                } & !(1 << bit);
                if target == 6 {
                    self.write_hl_timed_late(value);
                } else {
                    self.write_r8(target, value);
                }
                Ok(if target == 6 { 8 } else { 8 })
            }
            0xC0..=0xFF => {
                let bit = (opcode - 0xC0) / 8;
                let target = opcode & 0x07;
                let value = if target == 6 {
                    self.read_hl_timed_late()
                } else {
                    self.read_r8(target)
                } | (1 << bit);
                if target == 6 {
                    self.write_hl_timed_late(value);
                } else {
                    self.write_r8(target, value);
                }
                Ok(if target == 6 { 8 } else { 8 })
            }
        }
    }

    fn push16(&mut self, value: u16) -> Result<(), GbError> {
        let hi = (value >> 8) as u8;
        let lo = value as u8;
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, hi);
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, lo);
        Ok(())
    }

    fn pop16(&mut self) -> u16 {
        let lo = u16::from(self.read8(self.registers.sp));
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_add(1);
        let hi = u16::from(self.read8(self.registers.sp));
        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_add(1);
        lo | (hi << 8)
    }

    fn request_interrupt(&mut self, mask: u8) {
        let value = self.io[(IF_REGISTER - IO_START) as usize] | mask | 0xE0;
        self.io[(IF_REGISTER - IO_START) as usize] = value;
    }

    fn timer_bit_mask(&self) -> u16 {
        match self.io[(TIMER_TAC - IO_START) as usize] & 0x03 {
            0 => 1 << 9,
            1 => 1 << 3,
            2 => 1 << 5,
            _ => 1 << 7,
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
            self.tima_reload_state = Some(TimaReloadState::OverflowDelay(4));
        }
    }

    fn step_timer_cycle(&mut self) {
        let old_signal = self.timer_signal();
        self.div_counter = self.div_counter.wrapping_add(1);
        self.io[(TIMER_DIV - IO_START) as usize] = (self.div_counter >> 8) as u8;
        let new_signal = self.timer_signal();
        if old_signal && !new_signal {
            self.increment_tima();
        }

        if let Some(state) = self.tima_reload_state {
            match state {
                TimaReloadState::OverflowDelay(remaining) => {
                    if remaining <= 1 {
                        let tma = self.io[(TIMER_TMA - IO_START) as usize];
                        self.io[(TIMER_TIMA - IO_START) as usize] = tma;
                        self.request_interrupt(0x04);
                        self.tima_reload_state = Some(TimaReloadState::ReloadWindow(4));
                    } else {
                        self.tima_reload_state = Some(TimaReloadState::OverflowDelay(remaining - 1));
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
    }

    fn step_ppu_cycle(&mut self) {
        let lcdc = self.io[(LCDC_REGISTER - IO_START) as usize];
        if lcdc & 0x80 == 0 {
            self.ppu_cycle_counter = 0;
            self.io[(LY_REGISTER - IO_START) as usize] = 0;
            self.refresh_stat();
            return;
        }

        self.ppu_cycle_counter = self.ppu_cycle_counter.wrapping_add(1);
        self.refresh_stat();
        if self.ppu_cycle_counter < 456 {
            return;
        }

        self.ppu_cycle_counter = 0;
        let ly_index = (LY_REGISTER - IO_START) as usize;
        let next_ly = self.io[ly_index].wrapping_add(1);
        self.io[ly_index] = if next_ly > 153 { 0 } else { next_ly };
        if self.io[ly_index] == 144 {
            self.request_interrupt(0x01);
        }
        self.refresh_stat();
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

    fn service_interrupt(&mut self) -> Result<bool, GbError> {
        let pending = self.pending_interrupts();
        if pending == 0 {
            return Ok(false);
        }

        if !self.ime {
            return Ok(false);
        }

        if Self::highest_priority_interrupt(pending).is_none() {
            return Ok(false);
        }

        self.ime = false;
        let pc = self.registers.pc;
        let hi = (pc >> 8) as u8;
        let lo = pc as u8;

        self.tick_timers(8);

        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, hi);

        let pending_after_hi = self.pending_interrupts();
        let Some((selected_mask, vector)) = Self::highest_priority_interrupt(pending_after_hi) else {
            self.registers.pc = 0x0000;
            return Ok(true);
        };

        self.tick_timers(4);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, lo);

        let index = (IF_REGISTER - IO_START) as usize;
        self.io[index] = (self.io[index] & !selected_mask) | 0xE0;
        self.registers.pc = vector;
        self.tick_timers(4);
        Ok(true)
    }

    fn tick_timers(&mut self, cycles: u16) {
        self.cycle_counter += u64::from(cycles);
        for _ in 0..cycles {
            if let Some(source) = self.dma_pending_source {
                self.dma_pending_delay_cycles -= 1;
                if self.dma_pending_delay_cycles == 0 {
                    self.dma_pending_source = None;
                    self.begin_dma(source, 0);
                }
            }

            self.step_timer_cycle();
            self.step_ppu_cycle();
            if self.dma_delay_cycles > 0 {
                self.dma_delay_cycles -= 1;
            } else if self.dma_cycles_remaining > 0 {
                self.dma_cycles_remaining -= 1;
                self.dma_byte_cycles = self.dma_byte_cycles.wrapping_add(1);
                if self.dma_byte_cycles == 4 {
                    self.dma_byte_cycles = 0;
                    if self.dma_next_byte < 0xA0 {
                        self.oam[self.dma_next_byte as usize] =
                            self.dma_source_byte(self.dma_source.wrapping_add(self.dma_next_byte));
                        self.dma_next_byte += 1;
                    }
                }
            }
        }
    }

    fn execute_next_instruction(&mut self) -> Result<RunResult, GbError> {
        if self.ime_enable_delay > 0 {
            self.ime_enable_delay -= 1;
            if self.ime_enable_delay == 0 {
                self.ime = true;
            }
        }

        if self.halted {
            self.tick_timers(4);
            let pending = self.pending_interrupts();
            if pending == 0 {
                return Ok(RunResult {
                    stop_reason: StopReason::Halted,
                });
            }

            self.halted = false;
            if self.ime && self.service_interrupt()? {
                return Ok(RunResult {
                    stop_reason: StopReason::BreakpointHit,
                });
            }

            return Ok(RunResult {
                stop_reason: StopReason::StepComplete,
            });
        }

        if self.service_interrupt()? {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }

        self.pending_watchpoint = None;

        let pc = self.registers.pc;
        if self.has_pc_breakpoint(pc) {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }
        if self.has_opcode_breakpoint(self.peek8(pc)) {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }

        let opcode_pc = self.registers.pc;
        let opcode = self.fetch8();

        let cycles = match opcode {
            0x00 => 4,
            0x01 => {
                let value = self.fetch16();
                self.registers.set_bc(value);
                12
            }
            0x02 => {
                self.write8(self.registers.bc(), self.registers.a);
                8
            }
            0x03 => {
                self.maybe_trigger_oam_bug(self.registers.bc(), OamCorruptionKind::Write);
                let value = self.registers.bc().wrapping_add(1);
                self.registers.set_bc(value);
                8
            }
            0x04 => {
                self.registers.b = self.inc8(self.registers.b);
                4
            }
            0x3E => {
                self.registers.a = self.fetch8();
                8
            }
            0x05 => {
                self.registers.b = self.dec8(self.registers.b);
                4
            }
            0x06 => {
                self.registers.b = self.fetch8();
                8
            }
            0x07 => {
                let carry = self.registers.a & 0x80 != 0;
                self.registers.a = self.registers.a.rotate_left(1);
                self.set_flag(0x80, false);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                4
            }
            0x0F => {
                let carry = self.registers.a & 0x01 != 0;
                self.registers.a = self.registers.a.rotate_right(1);
                self.set_flag(0x80, false);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                4
            }
            0x09 => {
                self.add16_hl(self.registers.bc());
                8
            }
            0x0A => {
                self.registers.a = self.read8(self.registers.bc());
                8
            }
            0x0B => {
                self.maybe_trigger_oam_bug(self.registers.bc(), OamCorruptionKind::Write);
                let value = self.registers.bc().wrapping_sub(1);
                self.registers.set_bc(value);
                8
            }
            0x0C => {
                self.registers.c = self.inc8(self.registers.c);
                4
            }
            0x0D => {
                self.registers.c = self.dec8(self.registers.c);
                4
            }
            0x0E => {
                self.registers.c = self.fetch8();
                8
            }
            0x11 => {
                let value = self.fetch16();
                self.registers.set_de(value);
                12
            }
            0x12 => {
                self.write8(self.registers.de(), self.registers.a);
                8
            }
            0x13 => {
                self.maybe_trigger_oam_bug(self.registers.de(), OamCorruptionKind::Write);
                let value = self.registers.de().wrapping_add(1);
                self.registers.set_de(value);
                8
            }
            0x14 => {
                self.registers.d = self.inc8(self.registers.d);
                4
            }
            0x17 => {
                let carry_in = u8::from(self.flag_c());
                let carry_out = self.registers.a & 0x80 != 0;
                self.registers.a = (self.registers.a << 1) | carry_in;
                self.set_flag(0x80, false);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry_out);
                4
            }
            0x15 => {
                self.registers.d = self.dec8(self.registers.d);
                4
            }
            0x16 => {
                self.registers.d = self.fetch8();
                8
            }
            0x1A => {
                self.registers.a = self.read8(self.registers.de());
                8
            }
            0x1B => {
                self.maybe_trigger_oam_bug(self.registers.de(), OamCorruptionKind::Write);
                let value = self.registers.de().wrapping_sub(1);
                self.registers.set_de(value);
                8
            }
            0x1C => {
                self.registers.e = self.inc8(self.registers.e);
                4
            }
            0x1D => {
                self.registers.e = self.dec8(self.registers.e);
                4
            }
            0x1E => {
                self.registers.e = self.fetch8();
                8
            }
            0x18 => {
                self.jump_relative();
                12
            }
            0x1F => {
                let carry_in = if self.flag_c() { 0x80 } else { 0 };
                let carry_out = self.registers.a & 0x01 != 0;
                self.registers.a = (self.registers.a >> 1) | carry_in;
                self.set_flag(0x80, false);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry_out);
                4
            }
            0x19 => {
                self.add16_hl(self.registers.de());
                8
            }
            0x20 => {
                if !self.flag_z() {
                    self.jump_relative();
                    12
                } else {
                    self.fetch8();
                    8
                }
            }
            0x21 => {
                let value = self.fetch16();
                self.registers.set_hl(value);
                12
            }
            0x29 => {
                let hl = self.registers.hl();
                self.add16_hl(hl);
                8
            }
            0x23 => {
                self.maybe_trigger_oam_bug(self.registers.hl(), OamCorruptionKind::Write);
                let value = self.registers.hl().wrapping_add(1);
                self.registers.set_hl(value);
                8
            }
            0x22 => {
                let address = self.registers.hl();
                self.write8(address, self.registers.a);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_add(1));
                8
            }
            0x24 => {
                self.registers.h = self.inc8(self.registers.h);
                4
            }
            0x25 => {
                self.registers.h = self.dec8(self.registers.h);
                4
            }
            0x26 => {
                self.registers.h = self.fetch8();
                8
            }
            0x2A => {
                let address = self.registers.hl();
                self.registers.a = self.read8_with_kind(address, OamCorruptionKind::ReadWrite);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_add(1));
                8
            }
            0x2B => {
                self.maybe_trigger_oam_bug(self.registers.hl(), OamCorruptionKind::Write);
                let value = self.registers.hl().wrapping_sub(1);
                self.registers.set_hl(value);
                8
            }
            0x2F => {
                self.registers.a = !self.registers.a;
                self.set_flag(0x40, true);
                self.set_flag(0x20, true);
                4
            }
            0x2C => {
                self.registers.l = self.inc8(self.registers.l);
                4
            }
            0x2D => {
                self.registers.l = self.dec8(self.registers.l);
                4
            }
            0x2E => {
                self.registers.l = self.fetch8();
                8
            }
            0x31 => {
                self.registers.sp = self.fetch16();
                12
            }
            0x33 => {
                self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
                self.registers.sp = self.registers.sp.wrapping_add(1);
                8
            }
            0x39 => {
                self.add16_hl(self.registers.sp);
                8
            }
            0x3B => {
                self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
                self.registers.sp = self.registers.sp.wrapping_sub(1);
                8
            }
            0x34 => {
                let value = self.read8(self.registers.hl());
                let result = self.inc8(value);
                self.tick_timers(4);
                self.write8(self.registers.hl(), result);
                8
            }
            0x35 => {
                let value = self.read8(self.registers.hl());
                let result = self.dec8(value);
                self.tick_timers(4);
                self.write8(self.registers.hl(), result);
                8
            }
            0x30 => {
                if !self.flag_c() {
                    self.jump_relative();
                    12
                } else {
                    self.fetch8();
                    8
                }
            }
            0x28 => {
                if self.flag_z() {
                    self.jump_relative();
                    12
                } else {
                    self.fetch8();
                    8
                }
            }
            0x27 => {
                self.daa();
                4
            }
            0x38 => {
                if self.flag_c() {
                    self.jump_relative();
                    12
                } else {
                    self.fetch8();
                    8
                }
            }
            0x3A => {
                let address = self.registers.hl();
                self.registers.a = self.read8_with_kind(address, OamCorruptionKind::ReadWrite);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_sub(1));
                8
            }
            0x3C => {
                self.registers.a = self.inc8(self.registers.a);
                4
            }
            0x3D => {
                self.registers.a = self.dec8(self.registers.a);
                4
            }
            0x3F => {
                let carry = !self.flag_c();
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                4
            }
            0x32 => {
                let address = self.registers.hl();
                self.write8(address, self.registers.a);
                self.maybe_trigger_oam_bug(address, OamCorruptionKind::Write);
                self.registers.set_hl(address.wrapping_sub(1));
                8
            }
            0x36 => {
                let value = self.fetch8();
                self.write_hl_timed_late(value);
                8
            }
            0x40..=0x7F if opcode != 0x76 => {
                let dst = (opcode >> 3) & 0x07;
                let src = opcode & 0x07;
                let value = self.read_r8(src);
                self.write_r8(dst, value);
                if dst == 6 || src == 6 { 8 } else { 4 }
            }
            0x80..=0x87 => {
                let value = self.read_r8(opcode & 0x07);
                self.add8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0x88..=0x8F => {
                let value = self.read_r8(opcode & 0x07);
                self.adc8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0x90..=0x97 => {
                let value = self.read_r8(opcode & 0x07);
                self.sub8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0x98..=0x9F => {
                let value = self.read_r8(opcode & 0x07);
                self.sbc8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0xAF => {
                self.registers.a = 0;
                self.registers.f = 0x80;
                4
            }
            0xC1 => {
                let value = self.pop16();
                self.registers.set_bc(value);
                4
            }
            0xC0 => {
                if self.condition_true(0) {
                    self.tick_timers(8);
                    self.registers.pc = self.pop16();
                    self.tick_timers(4);
                    0
                } else {
                    8
                }
            }
            0xCB => self.execute_cb_prefixed()?,
            0xC9 => {
                self.tick_timers(4);
                self.registers.pc = self.pop16();
                self.tick_timers(4);
                0
            }
            0xC3 => {
                let address = self.fetch16_timed_late();
                self.tick_timers(4);
                self.registers.pc = address;
                4
            }
            0xC2 => {
                let address = self.fetch16_timed_late();
                if self.condition_true(0) {
                    self.registers.pc = address;
                    self.tick_timers(4);
                    4
                } else {
                    4
                }
            }
            0xC5 => {
                self.tick_timers(4);
                self.push16(self.registers.bc())?;
                4
            }
            0xC6 => {
                let value = self.fetch8();
                self.add8(value);
                8
            }
            0xC4 => {
                let address = self.fetch16_timed_late();
                if self.condition_true(0) {
                    self.tick_timers(4);
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    4
                } else {
                    4
                }
            }
            0xC8 => {
                if self.condition_true(1) {
                    self.tick_timers(8);
                    self.registers.pc = self.pop16();
                    self.tick_timers(4);
                    0
                } else {
                    8
                }
            }
            0xCA => {
                let address = self.fetch16_timed_late();
                if self.condition_true(1) {
                    self.registers.pc = address;
                    self.tick_timers(4);
                    4
                } else {
                    4
                }
            }
            0xCC => {
                let address = self.fetch16_timed_late();
                if self.condition_true(1) {
                    self.tick_timers(4);
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    4
                } else {
                    4
                }
            }
            0xCD => {
                let address = self.fetch16_timed_late();
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = address;
                4
            }
            0xCE => {
                let value = self.fetch8();
                self.adc8(value);
                8
            }
            0xC7 => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0000;
                4
            }
            0xCF => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0008;
                4
            }
            0xD7 => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0010;
                4
            }
            0xDF => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0018;
                4
            }
            0xD1 => {
                let value = self.pop16();
                self.registers.set_de(value);
                4
            }
            0xD0 => {
                if self.condition_true(2) {
                    self.tick_timers(8);
                    self.registers.pc = self.pop16();
                    self.tick_timers(4);
                    0
                } else {
                    8
                }
            }
            0xD5 => {
                self.tick_timers(4);
                self.push16(self.registers.de())?;
                4
            }
            0xD2 => {
                let address = self.fetch16_timed_late();
                if self.condition_true(2) {
                    self.registers.pc = address;
                    self.tick_timers(4);
                    4
                } else {
                    4
                }
            }
            0xD4 => {
                let address = self.fetch16_timed_late();
                if self.condition_true(2) {
                    self.tick_timers(4);
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    4
                } else {
                    4
                }
            }
            0xD6 => {
                let value = self.fetch8();
                self.sub8(value);
                8
            }
            0xDE => {
                let value = self.fetch8();
                self.sbc8(value);
                8
            }
            0xD8 => {
                if self.condition_true(3) {
                    self.tick_timers(8);
                    self.registers.pc = self.pop16();
                    self.tick_timers(4);
                    0
                } else {
                    8
                }
            }
            0xDA => {
                let address = self.fetch16_timed_late();
                if self.condition_true(3) {
                    self.registers.pc = address;
                    self.tick_timers(4);
                    4
                } else {
                    4
                }
            }
            0xDC => {
                let address = self.fetch16_timed_late();
                if self.condition_true(3) {
                    self.tick_timers(4);
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    4
                } else {
                    4
                }
            }
            0xEA => {
                let address = self.fetch16_timed_late();
                self.write8(address, self.registers.a);
                8
            }
            0xE0 => {
                let offset = self.fetch8_timed_late();
                self.write8(0xFF00 | u16::from(offset), self.registers.a);
                8
            }
            0xE2 => {
                self.write8(0xFF00 | u16::from(self.registers.c), self.registers.a);
                8
            }
            0xE1 => {
                let value = self.pop16();
                self.registers.set_hl(value);
                4
            }
            0xE5 => {
                self.tick_timers(4);
                self.push16(self.registers.hl())?;
                4
            }
            0xE6 => {
                let value = self.fetch8();
                self.and8(value);
                8
            }
            0xE7 => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0020;
                4
            }
            0xE8 => {
                let offset = self.fetch8_timed_late() as i8;
                self.tick_timers(8);
                self.registers.sp = self.add_sp_signed(offset);
                4
            }
            0xE9 => {
                self.registers.pc = self.registers.hl();
                4
            }
            0xEF => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0028;
                4
            }
            0xF6 => {
                let value = self.fetch8();
                self.or8(value);
                8
            }
            0xEE => {
                let value = self.fetch8();
                self.xor8(value);
                8
            }
            0xFA => {
                let address = self.fetch16_timed_late();
                self.registers.a = self.read8(address);
                8
            }
            0xF0 => {
                let offset = self.fetch8_timed_late();
                self.registers.a = self.read8(0xFF00 | u16::from(offset));
                8
            }
            0xF1 => {
                let value = self.pop16();
                self.registers.set_af(value);
                4
            }
            0xF8 => {
                let offset = self.fetch8_timed_late() as i8;
                self.tick_timers(4);
                let value = self.add_sp_signed(offset);
                self.registers.set_hl(value);
                4
            }
            0xF9 => {
                self.registers.sp = self.registers.hl();
                8
            }
            0xF3 => {
                self.ime = false;
                self.ime_enable_delay = 0;
                4
            }
            0xF2 => {
                self.registers.a = self.read8(0xFF00 | u16::from(self.registers.c));
                8
            }
            0xF5 => {
                self.tick_timers(4);
                self.push16(self.registers.af())?;
                4
            }
            0xF7 => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0030;
                4
            }
            0xFE => {
                let value = self.fetch8();
                self.cp8(value);
                8
            }
            0xFF => {
                self.tick_timers(4);
                self.push16(self.registers.pc)?;
                self.registers.pc = 0x0038;
                4
            }
            0xFB => {
                self.ime_enable_delay = 2;
                4
            }
            0xD9 => {
                self.tick_timers(4);
                self.registers.pc = self.pop16();
                self.tick_timers(4);
                self.ime = true;
                self.ime_enable_delay = 0;
                0
            }
            0x08 => {
                let address = self.fetch16();
                self.tick_timers(12);
                self.write8(address, self.registers.sp as u8);
                self.tick_timers(4);
                self.write8(address.wrapping_add(1), (self.registers.sp >> 8) as u8);
                4
            }
            0xA0..=0xA7 => {
                let value = self.read_r8(opcode & 0x07);
                self.and8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0xA8..=0xAE => {
                let value = self.read_r8(opcode & 0x07);
                self.xor8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0xB8..=0xBF => {
                let value = self.read_r8(opcode & 0x07);
                self.cp8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0xB0..=0xB7 => {
                let value = self.read_r8(opcode & 0x07);
                self.or8(value);
                if opcode & 0x07 == 6 { 8 } else { 4 }
            }
            0x37 => {
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, true);
                4
            }
            0x76 => {
                if self.ime_enable_delay == 1 {
                    self.ime = true;
                    self.ime_enable_delay = 0;
                }
                if !self.ime && self.pending_interrupts() != 0 {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
                4
            }
            _ => {
                return Err(GbError::UnsupportedOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
        };

        self.instruction_counter += 1;
        self.tick_timers(cycles);

        let stop_reason = if self.halted {
            StopReason::Halted
        } else if self.pending_watchpoint.is_some() {
            StopReason::WatchpointHit
        } else if self.has_pc_breakpoint(self.registers.pc) {
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
            let value = match current {
                0x0000..=ROM_BANK_0_END | 0x4000..=ROM_BANK_N_END => {
                    self.rom.get(current as usize).copied().unwrap_or(0xFF)
                }
                0xA000..=0xBFFF => self.eram[(current - 0xA000) as usize],
                VRAM_START..=VRAM_END => self.vram[(current - VRAM_START) as usize],
                WRAM_START..=WRAM_END => self.wram[(current - WRAM_START) as usize],
                ECHO_START..=ECHO_END => self.wram[(current - ECHO_START) as usize],
                OAM_START..=OAM_END => self.oam[(current - OAM_START) as usize],
                IO_START..=IO_END => self.io[(current - IO_START) as usize],
                HRAM_START..=HRAM_END => self.hram[(current - HRAM_START) as usize],
                IE_REGISTER => self.ie,
                _ => 0xFF,
            };
            bytes.push(value);
        }
        Some(bytes)
    }
}

impl Machine for GbMachine {
    type Error = GbError;

    fn control(&mut self) -> &mut dyn MachineControl<Error = Self::Error> {
        self
    }

    fn snapshot(&self) -> MachineSnapshot {
        MachineSnapshot {
            registers: self.registers.as_snapshot(),
            halted: self.halted,
            instruction_counter: self.instruction_counter,
        }
    }

    fn inspect_memory(&self, region: MemoryRegion, address: u32, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(address).ok()?;
        match region {
            MemoryRegion::Rom => self.rom.get(start..start.checked_add(len)?).map(|s| s.to_vec()),
            MemoryRegion::Ram => self
                .wram
                .get(start..start.checked_add(len)?)
                .map(|s| s.to_vec())
                .or_else(|| self.eram.get(start..start.checked_add(len)?).map(|s| s.to_vec())),
            MemoryRegion::Vram => self.vram.get(start..start.checked_add(len)?).map(|s| s.to_vec()),
            MemoryRegion::Oam => self.oam.get(start..start.checked_add(len)?).map(|s| s.to_vec()),
            MemoryRegion::AddressSpace(AddressSpace::System) => {
                self.system_memory_slice(u16::try_from(address).ok()?, len)
            }
        }
    }

    fn render_frame(&self, _target: RenderTarget) -> Result<FrameBuffer, Self::Error> {
        let mut frame = FrameBuffer::new_rgba(FRAME_WIDTH, FRAME_HEIGHT);

        for y in 0..FRAME_HEIGHT as usize {
            for x in 0..FRAME_WIDTH as usize {
                let tile_x = x / 8;
                let tile_y = y / 8;
                let tile_index = (tile_y * 20 + tile_x) % self.vram.len();
                let tile_value = self.vram[tile_index];
                let pixel_value = tile_value
                    .wrapping_add(self.registers.a)
                    .wrapping_add((x as u8) ^ (y as u8));
                let shade = pixel_value & 0b11;
                let intensity = match shade {
                    0 => 0xE0,
                    1 => 0xA8,
                    2 => 0x60,
                    _ => 0x18,
                };

                let offset = (y * FRAME_WIDTH as usize + x) * 4;
                frame.pixels_rgba8[offset] = intensity;
                frame.pixels_rgba8[offset + 1] = intensity;
                frame.pixels_rgba8[offset + 2] = intensity;
                frame.pixels_rgba8[offset + 3] = 0xFF;
            }
        }

        Ok(frame)
    }
}

impl MachineControl for GbMachine {
    type Error = GbError;

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.reset_state();
        Ok(())
    }

    fn run(&mut self) -> Result<RunResult, Self::Error> {
        for _ in 0..DEFAULT_RUN_LIMIT {
            let result = self.execute_next_instruction()?;
            if result.stop_reason != StopReason::StepComplete {
                return Ok(result);
            }
        }

        Ok(RunResult {
            stop_reason: StopReason::RunLimitReached,
        })
    }

    fn step_instruction(&mut self) -> Result<RunResult, Self::Error> {
        self.execute_next_instruction()
    }

    fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), Self::Error> {
        self.breakpoints.push(breakpoint);
        Ok(())
    }

    fn clear_breakpoints(&mut self) -> Result<(), Self::Error> {
        self.breakpoints.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use gbbrain_core::{Breakpoint, Machine, MachineControl, MemoryRegion, StopReason};

    use super::{
        GbMachine, IF_REGISTER, LCDC_REGISTER, LY_REGISTER, TIMER_DIV, TIMER_TAC, TIMER_TIMA,
        TIMER_TMA,
    };

    #[test]
    fn step_advances_pc_for_nop() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();

        let result = machine.step_instruction().unwrap();
        let snapshot = machine.snapshot();

        assert_eq!(result.stop_reason, StopReason::StepComplete);
        assert_eq!(snapshot.registers.pc, 0x0101);
        assert_eq!(snapshot.instruction_counter, 1);
    }

    #[test]
    fn program_counter_breakpoint_stops_run() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0x00;
        rom[0x101] = 0x00;

        let mut machine = GbMachine::new(rom).unwrap();
        machine.add_breakpoint(Breakpoint::ProgramCounter(0x0101)).unwrap();

        let result = machine.run().unwrap();

        assert_eq!(result.stop_reason, StopReason::BreakpointHit);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);
    }

    #[test]
    fn memory_write_watchpoint_triggers() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0x3E;
        rom[0x101] = 0x42;
        rom[0x102] = 0xEA;
        rom[0x103] = 0x00;
        rom[0x104] = 0xC0;

        let mut machine = GbMachine::new(rom).unwrap();
        machine.add_breakpoint(Breakpoint::MemoryWrite(0xC000)).unwrap();

        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::StepComplete);
        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::WatchpointHit);
        assert_eq!(
            machine.inspect_memory(MemoryRegion::Ram, 0, 1).unwrap(),
            vec![0x42]
        );
    }

    #[test]
    fn opcode_breakpoint_stops_before_execution() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0x00;
        rom[0x101] = 0x40;

        let mut machine = GbMachine::new(rom).unwrap();
        machine.add_breakpoint(Breakpoint::Opcode(0x40)).unwrap();

        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::StepComplete);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);

        let result = machine.run().unwrap();
        assert_eq!(result.stop_reason, StopReason::BreakpointHit);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);
    }

    #[test]
    fn ei_enables_interrupts_after_following_instruction() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0xFB;
        rom[0x101] = 0x00;
        rom[0x102] = 0x00;

        let mut machine = GbMachine::new(rom).unwrap();

        machine.step_instruction().unwrap();
        assert!(!machine.ime);
        assert_eq!(machine.ime_enable_delay, 2);

        machine.step_instruction().unwrap();
        assert!(!machine.ime);
        assert_eq!(machine.ime_enable_delay, 1);

        machine.step_instruction().unwrap();
        assert!(machine.ime);
        assert_eq!(machine.ime_enable_delay, 0);
    }

    #[test]
    fn halt_bug_repeats_next_opcode_when_interrupts_pending_and_ime_clear() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0x76;
        rom[0x101] = 0x00;
        rom[0x102] = 0x00;

        let mut machine = GbMachine::new(rom).unwrap();
        machine.ie = 0x01;
        machine.request_interrupt(0x01);

        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::StepComplete);
        assert!(!machine.halted);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);

        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::StepComplete);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);

        assert_eq!(machine.step_instruction().unwrap().stop_reason, StopReason::StepComplete);
        assert_eq!(machine.snapshot().registers.pc, 0x0102);
    }

    #[test]
    fn trace_entries_record_executed_instructions() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();

        machine.step_instruction().unwrap();
        machine.step_instruction().unwrap();

        let trace = machine.trace_entries();
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[0].pc, 0x0100);
        assert_eq!(trace[1].instruction_counter, 2);
    }

    #[test]
    fn serial_transfer_appends_output() {
        let mut rom = vec![0; 0x200];
        rom[0x100..0x10B].copy_from_slice(&[
            0x3E, b'A', 0xEA, 0x01, 0xFF, 0x3E, 0x81, 0xEA, 0x02, 0xFF, 0x76,
        ]);

        let mut machine = GbMachine::new(rom).unwrap();
        assert!(machine.run().is_ok());
        assert_eq!(machine.serial_output(), b"A");
    }

    #[test]
    fn timer_overflow_requests_interrupt() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();
        machine.write8(TIMER_DIV, 0x00);
        machine.write8(IF_REGISTER, 0x00);
        machine.write8(TIMER_TMA, 0x77);
        machine.write8(TIMER_TIMA, 0xFF);
        machine.write8(TIMER_TAC, 0x05);
        machine.tick_timers(16);

        assert_eq!(machine.read8(TIMER_TIMA), 0x00);
        assert_eq!(machine.read8(IF_REGISTER) & 0x04, 0);

        machine.tick_timers(4);

        assert_eq!(machine.read8(TIMER_TIMA), 0x77);
        assert_ne!(machine.read8(IF_REGISTER) & 0x04, 0);
    }

    #[test]
    fn lcd_enable_starts_first_visible_line_on_blargg_window() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();

        machine.write8(LCDC_REGISTER, 0x00);
        machine.write8(LCDC_REGISTER, 0x81);
        machine.tick_timers(112 * 4);
        assert_eq!(machine.read8(LY_REGISTER), 0);
        machine.tick_timers(4);
        assert_eq!(machine.read8(LY_REGISTER), 1);
    }

    #[test]
    #[ignore = "exploratory timing probe for Blargg init_timer; not a stable invariant yet"]
    fn blargg_init_timer_window_matches_expected_if_edge() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();

        machine.ie &= !0x04;
        machine.write8(TIMER_TMA, 0x00);
        machine.write8(TIMER_TAC, 0x05);
        machine.write8(IF_REGISTER, 0x00);
        machine.write8(TIMER_TIMA, 0xEC);

        machine.tick_timers(70);

        assert_eq!(machine.read8(IF_REGISTER) & 0x04, 0);

        let mut triggered_after = None;
        for extra in 1..=32 {
            machine.tick_timers(1);
            if machine.read8(IF_REGISTER) & 0x04 != 0 {
                triggered_after = Some(extra);
                break;
            }
        }

        assert_eq!(triggered_after, Some(7));
    }
}
