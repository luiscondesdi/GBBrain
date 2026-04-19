//! Minimal Game Boy DMG machine skeleton with usable debug plumbing.

use std::{collections::VecDeque, error::Error, fmt};

use gbbrain_core::{
    AddressSpace, Breakpoint, CpuRegisters, FrameBuffer, Machine, MachineControl, MachineSnapshot,
    MemoryRegion, RenderTarget, RunResult, StopReason,
};

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
enum WatchpointKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    ime_scheduled: bool,
    halted: bool,
    instruction_counter: u64,
    cycle_counter: u64,
    div_counter: u16,
    timer_counter: u16,
    pending_watchpoint: Option<WatchpointHit>,
    trace: VecDeque<TraceEntry>,
    serial_output: Vec<u8>,
}

impl GbMachine {
    pub fn new(rom: Vec<u8>) -> Result<Self, GbError> {
        if rom.is_empty() {
            return Err(GbError::EmptyRom);
        }

        Ok(Self {
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
            ime_scheduled: false,
            halted: false,
            instruction_counter: 0,
            cycle_counter: 0,
            div_counter: 0,
            timer_counter: 0,
            pending_watchpoint: None,
            trace: VecDeque::with_capacity(TRACE_CAPACITY),
            serial_output: Vec::new(),
        })
    }

    fn reset_state(&mut self) {
        self.eram.fill(0);
        self.vram.fill(0);
        self.wram.fill(0);
        self.oam.fill(0);
        self.io.fill(0);
        self.hram.fill(0);
        self.ie = 0;
        self.registers = Registers {
            sp: 0xFFFE,
            pc: 0x0100,
            ..Registers::default()
        };
        self.breakpoints.clear();
        self.ime = false;
        self.ime_scheduled = false;
        self.halted = false;
        self.instruction_counter = 0;
        self.cycle_counter = 0;
        self.div_counter = 0;
        self.timer_counter = 0;
        self.pending_watchpoint = None;
        self.trace.clear();
        self.serial_output.clear();
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

    fn read8(&mut self, address: u16) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
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
        match address {
            0xA000..=0xBFFF => self.eram[(address - 0xA000) as usize] = value,
            VRAM_START..=VRAM_END => self.vram[(address - VRAM_START) as usize] = value,
            WRAM_START..=WRAM_END => self.wram[(address - WRAM_START) as usize] = value,
            ECHO_START..=ECHO_END => self.wram[(address - ECHO_START) as usize] = value,
            OAM_START..=OAM_END => self.oam[(address - OAM_START) as usize] = value,
            IO_START..=IO_END => self.write_io(address, value),
            HRAM_START..=HRAM_END => self.hram[(address - HRAM_START) as usize] = value,
            IE_REGISTER => self.ie = value,
            _ => {}
        }
    }

    fn write_io(&mut self, address: u16, value: u8) {
        match address {
            TIMER_DIV => {
                self.io[(address - IO_START) as usize] = 0;
                self.div_counter = 0;
            }
            TIMER_TIMA | TIMER_TMA | TIMER_TAC | SERIAL_SB => {
                self.io[(address - IO_START) as usize] = value;
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
        let value = self.read8(self.registers.pc);
        self.registers.pc = self.registers.pc.wrapping_add(1);
        value
    }

    fn fetch16(&mut self) -> u16 {
        let lo = u16::from(self.fetch8());
        let hi = u16::from(self.fetch8());
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

    fn jump_relative(&mut self) {
        let offset = self.fetch8() as i8;
        self.registers.pc = self.registers.pc.wrapping_add_signed(i16::from(offset));
    }

    fn execute_cb_prefixed(&mut self) -> Result<u16, GbError> {
        let opcode = self.fetch8();
        match opcode {
            0x00..=0x07 => {
                let target = opcode & 0x07;
                let value = self.read_r8(target);
                let carry = value & 0x80 != 0;
                let result = value.rotate_left(1);
                self.write_r8(target, result);
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 16 } else { 8 })
            }
            0x08..=0x0F => {
                let target = opcode & 0x07;
                let value = self.read_r8(target);
                let carry = value & 0x01 != 0;
                let result = value.rotate_right(1);
                self.write_r8(target, result);
                self.set_flag(0x80, result == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, false);
                self.set_flag(0x10, carry);
                Ok(if target == 6 { 16 } else { 8 })
            }
            0x40..=0x7F => {
                let bit = (opcode - 0x40) / 8;
                let target = opcode & 0x07;
                let value = self.read_r8(target);
                self.set_flag(0x80, value & (1 << bit) == 0);
                self.set_flag(0x40, false);
                self.set_flag(0x20, true);
                Ok(if target == 6 { 12 } else { 8 })
            }
            _ => Err(GbError::UnsupportedOpcode {
                opcode: 0xCB,
                pc: self.registers.pc.wrapping_sub(2),
            }),
        }
    }

    fn push16(&mut self, value: u16) -> Result<(), GbError> {
        let hi = (value >> 8) as u8;
        let lo = value as u8;
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, hi);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        self.write8(self.registers.sp, lo);
        Ok(())
    }

    fn pop16(&mut self) -> u16 {
        let lo = u16::from(self.read8(self.registers.sp));
        self.registers.sp = self.registers.sp.wrapping_add(1);
        let hi = u16::from(self.read8(self.registers.sp));
        self.registers.sp = self.registers.sp.wrapping_add(1);
        lo | (hi << 8)
    }

    fn request_interrupt(&mut self, mask: u8) {
        let value = self.io[(IF_REGISTER - IO_START) as usize] | mask | 0xE0;
        self.io[(IF_REGISTER - IO_START) as usize] = value;
    }

    fn pending_interrupts(&self) -> u8 {
        self.ie & self.io[(IF_REGISTER - IO_START) as usize] & 0x1F
    }

    fn service_interrupt(&mut self) -> Result<bool, GbError> {
        let pending = self.pending_interrupts();
        if pending == 0 {
            return Ok(false);
        }

        if self.halted {
            self.halted = false;
        }

        if !self.ime {
            return Ok(false);
        }

        for (mask, vector) in INTERRUPT_VECTORS {
            if pending & mask != 0 {
                self.ime = false;
                let index = (IF_REGISTER - IO_START) as usize;
                self.io[index] = (self.io[index] & !mask) | 0xE0;
                let pc = self.registers.pc;
                self.push16(pc)?;
                self.registers.pc = *vector;
                self.cycle_counter += 20;
                self.tick_timers(20);
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn tick_timers(&mut self, cycles: u16) {
        self.cycle_counter += u64::from(cycles);

        self.div_counter = self.div_counter.wrapping_add(cycles);
        while self.div_counter >= 256 {
            self.div_counter -= 256;
            let index = (TIMER_DIV - IO_START) as usize;
            self.io[index] = self.io[index].wrapping_add(1);
        }

        let tac = self.io[(TIMER_TAC - IO_START) as usize];
        if tac & 0x04 == 0 {
            return;
        }

        let threshold = match tac & 0x03 {
            0 => 1024,
            1 => 16,
            2 => 64,
            _ => 256,
        };

        self.timer_counter = self.timer_counter.wrapping_add(cycles);
        while self.timer_counter >= threshold {
            self.timer_counter -= threshold;
            let tima_index = (TIMER_TIMA - IO_START) as usize;
            let tma = self.io[(TIMER_TMA - IO_START) as usize];
            let (next, overflow) = self.io[tima_index].overflowing_add(1);
            self.io[tima_index] = if overflow { tma } else { next };
            if overflow {
                self.request_interrupt(0x04);
            }
        }
    }

    fn execute_next_instruction(&mut self) -> Result<RunResult, GbError> {
        if self.ime_scheduled {
            self.ime = true;
            self.ime_scheduled = false;
        }

        if self.service_interrupt()? {
            return Ok(RunResult {
                stop_reason: StopReason::BreakpointHit,
            });
        }

        if self.halted {
            return Ok(RunResult {
                stop_reason: StopReason::Halted,
            });
        }

        self.pending_watchpoint = None;

        let pc = self.registers.pc;
        if self.has_pc_breakpoint(pc) {
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
            0x0A => {
                self.registers.a = self.read8(self.registers.bc());
                8
            }
            0x0B => {
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
                let value = self.registers.de().wrapping_add(1);
                self.registers.set_de(value);
                8
            }
            0x14 => {
                self.registers.d = self.inc8(self.registers.d);
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
            0x23 => {
                let value = self.registers.hl().wrapping_add(1);
                self.registers.set_hl(value);
                8
            }
            0x22 => {
                let address = self.registers.hl();
                self.write8(address, self.registers.a);
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
                self.registers.a = self.read8(address);
                self.registers.set_hl(address.wrapping_add(1));
                8
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
                self.registers.sp = self.registers.sp.wrapping_add(1);
                8
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
            0x3A => {
                let address = self.registers.hl();
                self.registers.a = self.read8(address);
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
            0x32 => {
                let address = self.registers.hl();
                self.write8(address, self.registers.a);
                self.registers.set_hl(address.wrapping_sub(1));
                8
            }
            0x36 => {
                let value = self.fetch8();
                self.write8(self.registers.hl(), value);
                12
            }
            0x40..=0x7F if opcode != 0x76 => {
                let dst = (opcode >> 3) & 0x07;
                let src = opcode & 0x07;
                let value = self.read_r8(src);
                self.write_r8(dst, value);
                if dst == 6 || src == 6 { 8 } else { 4 }
            }
            0xAF => {
                self.registers.a = 0;
                self.registers.f = 0x80;
                4
            }
            0xC1 => {
                let value = self.pop16();
                self.registers.set_bc(value);
                12
            }
            0xC0 => {
                if self.condition_true(0) {
                    self.registers.pc = self.pop16();
                    20
                } else {
                    8
                }
            }
            0xCB => self.execute_cb_prefixed()?,
            0xC9 => {
                self.registers.pc = self.pop16();
                16
            }
            0xC3 => {
                self.registers.pc = self.fetch16();
                16
            }
            0xC2 => {
                let address = self.fetch16();
                if self.condition_true(0) {
                    self.registers.pc = address;
                    16
                } else {
                    12
                }
            }
            0xC5 => {
                self.push16(self.registers.bc())?;
                16
            }
            0xC4 => {
                let address = self.fetch16();
                if self.condition_true(0) {
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    24
                } else {
                    12
                }
            }
            0xC8 => {
                if self.condition_true(1) {
                    self.registers.pc = self.pop16();
                    20
                } else {
                    8
                }
            }
            0xCA => {
                let address = self.fetch16();
                if self.condition_true(1) {
                    self.registers.pc = address;
                    16
                } else {
                    12
                }
            }
            0xCC => {
                let address = self.fetch16();
                if self.condition_true(1) {
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    24
                } else {
                    12
                }
            }
            0xCD => {
                let address = self.fetch16();
                self.push16(self.registers.pc)?;
                self.registers.pc = address;
                24
            }
            0xD1 => {
                let value = self.pop16();
                self.registers.set_de(value);
                12
            }
            0xD0 => {
                if self.condition_true(2) {
                    self.registers.pc = self.pop16();
                    20
                } else {
                    8
                }
            }
            0xD5 => {
                self.push16(self.registers.de())?;
                16
            }
            0xD2 => {
                let address = self.fetch16();
                if self.condition_true(2) {
                    self.registers.pc = address;
                    16
                } else {
                    12
                }
            }
            0xD4 => {
                let address = self.fetch16();
                if self.condition_true(2) {
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    24
                } else {
                    12
                }
            }
            0xD6 => {
                let value = self.fetch8();
                self.sub8(value);
                8
            }
            0xD8 => {
                if self.condition_true(3) {
                    self.registers.pc = self.pop16();
                    20
                } else {
                    8
                }
            }
            0xDA => {
                let address = self.fetch16();
                if self.condition_true(3) {
                    self.registers.pc = address;
                    16
                } else {
                    12
                }
            }
            0xDC => {
                let address = self.fetch16();
                if self.condition_true(3) {
                    self.push16(self.registers.pc)?;
                    self.registers.pc = address;
                    24
                } else {
                    12
                }
            }
            0xEA => {
                let address = self.fetch16();
                self.write8(address, self.registers.a);
                16
            }
            0xE0 => {
                let offset = self.fetch8();
                self.write8(0xFF00 | u16::from(offset), self.registers.a);
                12
            }
            0xE2 => {
                self.write8(0xFF00 | u16::from(self.registers.c), self.registers.a);
                8
            }
            0xE1 => {
                let value = self.pop16();
                self.registers.set_hl(value);
                12
            }
            0xE5 => {
                self.push16(self.registers.hl())?;
                16
            }
            0xE6 => {
                let value = self.fetch8();
                self.and8(value);
                8
            }
            0xE9 => {
                self.registers.pc = self.registers.hl();
                4
            }
            0xFA => {
                let address = self.fetch16();
                self.registers.a = self.read8(address);
                16
            }
            0xF0 => {
                let offset = self.fetch8();
                self.registers.a = self.read8(0xFF00 | u16::from(offset));
                12
            }
            0xF1 => {
                let value = self.pop16();
                self.registers.set_af(value);
                12
            }
            0xF9 => {
                self.registers.sp = self.registers.hl();
                8
            }
            0xF3 => {
                self.ime = false;
                self.ime_scheduled = false;
                4
            }
            0xF2 => {
                self.registers.a = self.read8(0xFF00 | u16::from(self.registers.c));
                8
            }
            0xF5 => {
                self.push16(self.registers.af())?;
                16
            }
            0xFE => {
                let value = self.fetch8();
                self.cp8(value);
                8
            }
            0xFB => {
                self.ime_scheduled = true;
                4
            }
            0xD9 => {
                self.registers.pc = self.pop16();
                self.ime = true;
                self.ime_scheduled = false;
                16
            }
            0x08 => {
                let address = self.fetch16();
                self.write8(address, self.registers.sp as u8);
                self.write8(address.wrapping_add(1), (self.registers.sp >> 8) as u8);
                20
            }
            0xA0..=0xA7 => {
                let value = self.read_r8(opcode & 0x07);
                self.and8(value);
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
            0x76 => {
                self.halted = true;
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

    use super::{GbMachine, IF_REGISTER, TIMER_TAC, TIMER_TIMA, TIMER_TMA};

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
        machine.write8(TIMER_TMA, 0x77);
        machine.write8(TIMER_TIMA, 0xFF);
        machine.write8(TIMER_TAC, 0x05);
        machine.tick_timers(16);

        assert_eq!(machine.read8(TIMER_TIMA), 0x77);
        assert_ne!(machine.read8(IF_REGISTER) & 0x04, 0);
    }
}
