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
    rom_bank: u16,
    model: GbModel,
    eram: [u8; 0x2000],
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
    dma_source: u16,
    dma_active: bool,
    dma_requested_source: Option<u16>,
    dma_starting_source: Option<u16>,
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
    rom_bank: u16,
    model: GbModel,
    eram: Vec<u8>,
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
    dma_source: u16,
    dma_active: bool,
    dma_requested_source: Option<u16>,
    dma_starting_source: Option<u16>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ExecState {
    Running,
    Halt,
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
    pub fn new(rom: Vec<u8>) -> Result<Self, GbError> {
        Self::new_with_model(rom, GbModel::Dmg)
    }

    pub fn new_with_model(rom: Vec<u8>, model: GbModel) -> Result<Self, GbError> {
        if rom.is_empty() {
            return Err(GbError::EmptyRom);
        }

        let mut machine = Self {
            rom,
            rom_bank: 1,
            model,
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
            prefetched_pc: None,
            prefetched_opcode: None,
            breakpoints: Vec::new(),
            ime: false,
            exec_state: ExecState::Running,
            instruction_counter: 0,
            cycle_counter: 0,
            div_counter: 0,
            tima_reload_state: None,
            ppu_cycle_counter: 0,
            dma_source: 0,
            dma_active: false,
            dma_requested_source: None,
            dma_starting_source: None,
            dma_next_byte: 0,
            pending_watchpoint: None,
            trace: VecDeque::with_capacity(TRACE_CAPACITY),
            serial_output: Vec::new(),
        };
        machine.reset_state();
        Ok(machine)
    }

    fn reset_state(&mut self) {
        self.rom_bank = 1;
        self.eram.fill(0);
        self.vram.fill(0);
        self.wram.fill(0);
        self.oam.fill(0);
        self.io.fill(0xFF);
        self.hram.fill(0);
        self.ie = 0;
        self.registers = self.model_boot_registers();
        self.prefetched_pc = None;
        self.prefetched_opcode = None;
        self.breakpoints.clear();
        self.ime = false;
        self.exec_state = ExecState::Running;
        self.instruction_counter = 0;
        self.cycle_counter = 0;
        self.div_counter = self.model_boot_div_counter();
        self.tima_reload_state = None;
        self.ppu_cycle_counter = 0;
        self.dma_source = 0;
        self.dma_active = false;
        self.dma_requested_source = None;
        self.dma_starting_source = None;
        self.dma_next_byte = 0;
        self.pending_watchpoint = None;
        self.trace.clear();
        self.serial_output.clear();
        self.io[0x00] = self.model_boot_p1();
        self.io[(SERIAL_SB - IO_START) as usize] = 0x00;
        self.io[(SERIAL_SC - IO_START) as usize] = 0x7E;
        self.io[(TIMER_DIV - IO_START) as usize] = (self.div_counter >> 8) as u8;
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
        self.io[0x26] = self.model_boot_nr52(); // NR52
        self.io[(LCDC_REGISTER - IO_START) as usize] = self.model_boot_lcdc();
        self.io[(STAT_REGISTER - IO_START) as usize] = self.model_boot_stat();
        self.io[0x42] = 0x00; // SCY
        self.io[0x43] = 0x00; // SCX
        self.io[(LY_REGISTER - IO_START) as usize] = self.model_boot_ly();
        self.io[0x45] = self.model_boot_lyc();
        self.io[(DMA_REGISTER - IO_START) as usize] = 0xFF;
        self.io[0x47] = 0xFC; // BGP
        self.io[0x48] = self.model_boot_obp0();
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

    pub fn model(&self) -> GbModel {
        self.model
    }

    pub fn read_system_address(&mut self, address: u16) -> u8 {
        self.read8(address)
    }

    pub fn write_system_address(&mut self, address: u16, value: u8) {
        self.write8(address, value);
    }

    fn model_boot_registers(&self) -> Registers {
        match self.model {
            GbModel::Dmg0 => Registers {
                a: 0x01,
                f: 0x00,
                b: 0xFF,
                c: 0x13,
                d: 0x00,
                e: 0xC1,
                h: 0x84,
                l: 0x03,
                sp: 0xFFFE,
                pc: 0x0100,
            },
            GbModel::Dmg => Registers {
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
            },
            GbModel::Mgb => Registers {
                a: 0xFF,
                f: 0xB0,
                b: 0x00,
                c: 0x13,
                d: 0x00,
                e: 0xD8,
                h: 0x01,
                l: 0x4D,
                sp: 0xFFFE,
                pc: 0x0100,
            },
            GbModel::Sgb => Registers {
                a: 0x01,
                f: 0x00,
                b: 0x00,
                c: 0x14,
                d: 0x00,
                e: 0x00,
                h: 0xC0,
                l: 0x60,
                sp: 0xFFFE,
                pc: 0x0100,
            },
            GbModel::Sgb2 => Registers {
                a: 0xFF,
                f: 0x00,
                b: 0x00,
                c: 0x14,
                d: 0x00,
                e: 0x00,
                h: 0xC0,
                l: 0x60,
                sp: 0xFFFE,
                pc: 0x0100,
            },
        }
    }

    fn model_boot_div_counter(&self) -> u16 {
        match self.model {
            GbModel::Dmg0 => 0x18D0,
            GbModel::Dmg => 0xABD0,
            GbModel::Mgb => 0xABD0,
            GbModel::Sgb | GbModel::Sgb2 => 0xD8D0,
        }
    }

    fn model_boot_p1(&self) -> u8 {
        match self.model {
            GbModel::Sgb | GbModel::Sgb2 => 0xFF,
            _ => 0xCF,
        }
    }

    fn model_boot_lcdc(&self) -> u8 {
        match self.model {
            GbModel::Dmg0 => 0x91,
            GbModel::Dmg | GbModel::Mgb => 0x91,
            GbModel::Sgb | GbModel::Sgb2 => 0x91,
        }
    }

    fn model_boot_stat(&self) -> u8 {
        match self.model {
            GbModel::Dmg0 => 0x83,
            GbModel::Dmg | GbModel::Mgb => 0x80,
            GbModel::Sgb | GbModel::Sgb2 => 0x80,
        }
    }

    fn model_boot_ly(&self) -> u8 {
        match self.model {
            GbModel::Dmg0 => 0x01,
            _ => 0x00,
        }
    }

    fn model_boot_lyc(&self) -> u8 {
        match self.model {
            GbModel::Dmg0 => 0x00,
            GbModel::Dmg | GbModel::Mgb => 0x0A,
            GbModel::Sgb | GbModel::Sgb2 => 0x00,
        }
    }

    fn model_boot_obp0(&self) -> u8 {
        match self.model {
            GbModel::Sgb | GbModel::Sgb2 => 0x00,
            _ => 0xFF,
        }
    }

    fn model_boot_nr52(&self) -> u8 {
        match self.model {
            GbModel::Sgb | GbModel::Sgb2 => 0xF0,
            _ => 0xF1,
        }
    }

    fn set_exec_state(&mut self, state: ExecState) {
        self.exec_state = state;
    }

    pub fn save_state(&self) -> Result<Vec<u8>, GbError> {
        let state = SnapshotState {
            rom: self.rom.clone(),
            rom_bank: self.rom_bank,
            model: self.model,
            eram: self.eram.to_vec(),
            vram: self.vram.to_vec(),
            wram: self.wram.to_vec(),
            oam: self.oam.to_vec(),
            io: self.io.to_vec(),
            hram: self.hram.to_vec(),
            ie: self.ie,
            registers: self.registers,
            prefetched_pc: self.prefetched_pc,
            prefetched_opcode: self.prefetched_opcode,
            breakpoints: self
                .breakpoints
                .iter()
                .copied()
                .map(SnapshotBreakpoint::from)
                .collect(),
            ime: self.ime,
            ime_enable_delay: 0,
            exec_state: self.exec_state,
            halted: matches!(self.exec_state, ExecState::Halt),
            halt_bug: false,
            instruction_counter: self.instruction_counter,
            cycle_counter: self.cycle_counter,
            div_counter: self.div_counter,
            tima_reload_state: self.tima_reload_state,
            ppu_cycle_counter: self.ppu_cycle_counter,
            dma_source: self.dma_source,
            dma_active: self.dma_active,
            dma_requested_source: self.dma_requested_source,
            dma_starting_source: self.dma_starting_source,
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

        let mut machine = Self::new_with_model(state.rom, state.model)?;
        machine.rom_bank = state.rom_bank;
        machine.eram.copy_from_slice(&state.eram[..0x2000]);
        machine.vram.copy_from_slice(&state.vram[..0x2000]);
        machine.wram.copy_from_slice(&state.wram[..0x2000]);
        machine.oam.copy_from_slice(&state.oam[..0xA0]);
        machine.io.copy_from_slice(&state.io[..0x80]);
        machine.hram.copy_from_slice(&state.hram[..0x7F]);
        machine.ie = state.ie;
        machine.registers = state.registers;
        machine.prefetched_pc = state.prefetched_pc;
        machine.prefetched_opcode = state.prefetched_opcode;
        machine.breakpoints = state.breakpoints.into_iter().map(Breakpoint::from).collect();
        machine.ime = state.ime;
        machine.exec_state = if state.halted { ExecState::Halt } else { state.exec_state };
        machine.instruction_counter = state.instruction_counter;
        machine.cycle_counter = state.cycle_counter;
        machine.div_counter = state.div_counter;
        machine.tima_reload_state = state.tima_reload_state;
        machine.ppu_cycle_counter = state.ppu_cycle_counter;
        machine.dma_source = state.dma_source;
        machine.dma_active = state.dma_active;
        machine.dma_requested_source = state.dma_requested_source;
        machine.dma_starting_source = state.dma_starting_source;
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

    fn read8_with_kind(&mut self, address: u16, _kind: OamCorruptionKind) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
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
            0x0000..=ROM_BANK_0_END => {
                self.rom.get(address as usize).copied().unwrap_or(0xFF)
            }
            0x4000..=ROM_BANK_N_END => {
                let offset = u32::from(self.rom_bank) * 0x4000 + u32::from(address - 0x4000);
                self.rom.get(offset as usize).copied().unwrap_or(0xFF)
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

    fn read8_without_oam_bug(&mut self, address: u16) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
            return 0xFF;
        }
        if self.ppu_mode() == 3 && matches!(address, VRAM_START..=VRAM_END | OAM_START..=OAM_END) {
            return 0xFF;
        }
        self.peek8(address)
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
            0x2000..=0x2FFF => {
                self.rom_bank = (self.rom_bank & 0x100) | u16::from(value);
            }
            0x3000..=0x3FFF => {
                self.rom_bank = (self.rom_bank & 0x0FF) | (u16::from(value & 0x01) << 8);
            }
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
        self.dma_active
    }

    fn step_dma_mcycle(&mut self) {
        if self.dma_active {
            if self.dma_next_byte < 0xA0 {
                self.oam[self.dma_next_byte as usize] =
                    self.dma_source_byte(self.dma_source.wrapping_add(self.dma_next_byte));
                self.dma_next_byte += 1;
            }
            if self.dma_next_byte >= 0xA0 {
                self.dma_active = false;
            }
        }

        if let Some(source) = self.dma_starting_source.take() {
            self.dma_source = source;
            self.dma_active = true;
            self.dma_next_byte = 0;
        }

        if let Some(source) = self.dma_requested_source.take() {
            self.dma_starting_source = Some(source);
        }
    }

    fn dma_source_byte(&self, address: u16) -> u8 {
        match address {
            0x0000..=ROM_BANK_0_END => {
                self.rom.get(address as usize).copied().unwrap_or(0xFF)
            }
            0x4000..=ROM_BANK_N_END => {
                let offset = u32::from(self.rom_bank) * 0x4000 + u32::from(address - 0x4000);
                self.rom.get(offset as usize).copied().unwrap_or(0xFF)
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
                self.dma_requested_source = Some(source);
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
        self.registers.pc = self.registers.pc.wrapping_add(1);
        value
    }

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

    fn prefetch_opcode_cycle(&mut self, address: u16, advance_pc: bool, allow_interrupt_dispatch: bool) {
        self.tick_mcycle();
        self.prefetched_pc = Some(address);
        self.prefetched_opcode = Some(self.read8(address));
        if allow_interrupt_dispatch && self.ime && self.pending_interrupts() != 0 {
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
        if let (Some(pc), Some(opcode)) = (self.prefetched_pc.take(), self.prefetched_opcode.take()) {
            (pc, opcode)
        } else {
            let opcode_pc = self.registers.pc;
            let opcode = self.fetch8();
            (opcode_pc, opcode)
        }
    }

    fn tick_mcycle(&mut self) {
        self.tick_timers(4);
    }

    fn fetch8_cycle(&mut self) -> u8 {
        self.tick_mcycle();
        let value = self.read8(self.registers.pc);
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
        self.tick_mcycle();
        self.read8(address)
    }

    fn write_cycle(&mut self, address: u16, value: u8) {
        self.tick_mcycle();
        self.write8(address, value);
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
        self.set_flag(0x10, (u16::from(a) + u16::from(value) + u16::from(carry)) > 0xFF);
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

    fn execute_opcode_00_3f(&mut self, opcode: u8, opcode_pc: u16) -> Result<(), GbError> {
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
            _ => {
                return Err(GbError::UnsupportedOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
        }
        Ok(())
    }

    fn execute_opcode_40_bf(&mut self, opcode: u8, opcode_pc: u16) -> Result<(), GbError> {
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
            0x80..=0xBF => self.exec_decoded_alu(Self::decode_r8_in(opcode & 0x07), Self::decode_alu_op(opcode)),
            _ => {
                return Err(GbError::UnsupportedOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
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
            0xD4 => self.exec_call_condition_immediate(condition)?,
            0xD5 => self.push_stack_reg16_prefetch(stack_reg)?,
            0xD6 => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xD7 => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xD8 => self.exec_ret_condition_prefetch(condition),
            0xD9 => self.exec_reti(),
            0xDA => self.exec_jp_condition_immediate(condition),
            0xDC => self.exec_call_condition_immediate(condition)?,
            0xDE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xDF => self.rst_to(Self::decode_rst_vector(opcode))?,
            0xE0 => {
                self.exec_load8(Out8::Addr(AddrMode8::ZeroPageImm), In8::Reg(Reg8Id::A));
            }
            0xE1 => self.pop_stack_reg16_prefetch(stack_reg),
            0xE2 => {
                self.exec_load8(Out8::Addr(AddrMode8::ZeroPageC), In8::Reg(Reg8Id::A));
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
                self.prefetch_no_interrupt_cycle(self.registers.pc);
                self.ime = true;
            }
            0xFE => self.exec_decoded_alu(In8::Imm8, Self::decode_alu_op(opcode)),
            0xFF => self.rst_to(Self::decode_rst_vector(opcode))?,
            _ => {
                return Err(GbError::UnsupportedOpcode {
                    opcode,
                    pc: opcode_pc,
                });
            }
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
        self.tick_mcycle();
        let lo = u16::from(self.read8_without_oam_bug(self.registers.sp));
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
        self.registers.sp = self.registers.sp.wrapping_add(1);
        self.tick_mcycle();
        let hi = u16::from(self.read8_without_oam_bug(self.registers.sp));
        self.maybe_trigger_oam_bug(self.registers.sp, OamCorruptionKind::Write);
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

    fn execute_interrupt_dispatch(&mut self) -> Result<bool, GbError> {
        let pending = self.pending_interrupts();
        let Some(_) = Self::highest_priority_interrupt(pending) else {
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

        let pending_after_hi = self.pending_interrupts();
        let Some((selected_mask, vector)) = Self::highest_priority_interrupt(pending_after_hi) else {
            self.registers.pc = 0x0000;
            self.set_exec_state(ExecState::Running);
            return Ok(true);
        };

        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write_cycle(self.registers.sp, lo);

        let index = (IF_REGISTER - IO_START) as usize;
        self.io[index] = (self.io[index] & !selected_mask) | 0xE0;
        self.registers.pc = vector;
        self.prefetch_next_cycle(self.registers.pc);
        self.set_exec_state(ExecState::Running);
        Ok(true)
    }

    fn tick_timers(&mut self, cycles: u16) {
        for _ in 0..cycles {
            self.cycle_counter += 1;
            self.step_timer_cycle();
            self.step_ppu_cycle();
            if self.cycle_counter % 4 == 0 {
                self.step_dma_mcycle();
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
            let value = match current {
                0x0000..=ROM_BANK_0_END => {
                    self.rom.get(current as usize).copied().unwrap_or(0xFF)
                }
                0x4000..=ROM_BANK_N_END => {
                    let offset = u32::from(self.rom_bank) * 0x4000 + u32::from(current - 0x4000);
                    self.rom.get(offset as usize).copied().unwrap_or(0xFF)
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
            registers: self.snapshot_registers(),
            halted: matches!(self.exec_state, ExecState::Halt),
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
        ExecState, GbMachine, IF_REGISTER, LCDC_REGISTER, LY_REGISTER, TIMER_DIV, TIMER_TAC, TIMER_TIMA,
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
    fn direct_system_address_read_write_works() {
        let rom = vec![0; 0x200];
        let mut machine = GbMachine::new(rom).unwrap();

        machine.write_system_address(0xC000, 0x42);

        assert_eq!(machine.read_system_address(0xC000), 0x42);
        assert_eq!(
            machine.inspect_memory(MemoryRegion::AddressSpace(gbbrain_core::AddressSpace::System), 0xC000, 1)
                .unwrap(),
            vec![0x42]
        );
    }

    #[test]
    fn ei_enables_interrupts_after_following_instruction() {
        let mut rom = vec![0; 0x200];
        rom[0x100] = 0xFB;
        rom[0x101] = 0x00;
        rom[0x102] = 0x00;

        let mut machine = GbMachine::new(rom).unwrap();
        machine.ie = 0x01;
        machine.request_interrupt(0x01);

        machine.step_instruction().unwrap();
        assert!(machine.ime);
        assert_eq!(machine.snapshot().registers.pc, 0x0101);

        machine.step_instruction().unwrap();
        assert!(machine.ime);
        assert_eq!(machine.snapshot().registers.pc, 0x0102);

        machine.step_instruction().unwrap();
        assert!(!machine.ime);
        assert_eq!(machine.snapshot().registers.pc, 0x0040);
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
        assert!(!matches!(machine.exec_state, ExecState::Halt));
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
