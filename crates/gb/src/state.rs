use super::*;

impl GbMachine {
    pub fn new(rom: Vec<u8>) -> Result<Self, GbError> {
        Self::new_with_model(rom, GbModel::Dmg)
    }

    pub fn new_with_model(rom: Vec<u8>, model: GbModel) -> Result<Self, GbError> {
        Self::new_with_model_and_bootrom(rom, model, None)
    }

    pub fn new_with_model_and_bootrom(
        rom: Vec<u8>,
        model: GbModel,
        bootrom: Option<Vec<u8>>,
    ) -> Result<Self, GbError> {
        if rom.is_empty() {
            return Err(GbError::EmptyRom);
        }

        let bootrom = match bootrom {
            Some(bytes) => {
                if bytes.len() != 0x100 {
                    return Err(GbError::InvalidBootromSize(bytes.len()));
                }
                let mut data = [0; 0x100];
                data.copy_from_slice(&bytes);
                Some(Box::new(data))
            }
            None => Some(Box::new(Self::synthetic_bootrom(model))),
        };

        let mut machine = Self {
            cartridge: Cartridge::new(rom)?,
            bootrom_active: bootrom.is_some(),
            bootrom,
            model,
            joypad: JoypadState { pressed: 0 },
            vram: [0; 0x2000],
            wram: [0; 0x2000],
            oam: [0; 0xA0],
            io: [0; 0x80],
            hram: [0; 0x7F],
            ie: 0,
            registers: Registers::default(),
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
            ppu_mode: PpuMode::AccessOam,
            ppu_mode_cycles_remaining: PPU_ACCESS_OAM_CYCLES,
            dma_source: 0,
            dma_active: false,
            dma_requested_source: None,
            dma_starting_source: None,
            dma_next_byte: 0,
            pending_t34_interrupts: 0,
            pending_watchpoint: None,
            trace: VecDeque::with_capacity(TRACE_CAPACITY),
            serial_output: Vec::new(),
        };
        machine.reset_state();
        Ok(machine)
    }

    pub(crate) fn reset_state(&mut self) {
        let bootrom_active = self.bootrom.is_some();
        self.cartridge.reset();
        self.vram.fill(0);
        self.wram.fill(0);
        self.oam.fill(0);
        self.io.fill(0xFF);
        self.hram.fill(0);
        self.ie = 0;
        self.registers = Registers::default();
        self.joypad = JoypadState { pressed: 0 };
        self.prefetched_pc = Some(self.registers.pc);
        self.prefetched_opcode = Some(0x00);
        self.breakpoints.clear();
        self.ime = false;
        self.exec_state = ExecState::Running;
        self.instruction_counter = 0;
        self.cycle_counter = 0;
        self.div_counter = 0;
        self.tima_reload_state = None;
        self.ppu_cycle_counter = 0;
        self.ppu_mode = PpuMode::AccessOam;
        self.ppu_mode_cycles_remaining = PPU_ACCESS_OAM_CYCLES;
        self.bootrom_active = bootrom_active;
        self.dma_source = 0;
        self.dma_active = false;
        self.dma_requested_source = None;
        self.dma_starting_source = None;
        self.dma_next_byte = 0;
        self.pending_t34_interrupts = 0;
        self.pending_watchpoint = None;
        self.trace.clear();
        self.serial_output.clear();
        self.io[0x00] = Self::model_boot_p1(self.model);
        self.io[(SERIAL_SB - IO_START) as usize] = 0x00;
        self.io[(SERIAL_SC - IO_START) as usize] = 0x7E;
        self.io[(TIMER_DIV - IO_START) as usize] = 0x00;
        self.io[(TIMER_TIMA - IO_START) as usize] = 0x00;
        self.io[(TIMER_TMA - IO_START) as usize] = 0x00;
        self.io[(TIMER_TAC - IO_START) as usize] = 0xF8;
        self.io[(IF_REGISTER - IO_START) as usize] = 0xE0;
        self.io[(LCDC_REGISTER - IO_START) as usize] = 0x00;
        self.io[(STAT_REGISTER - IO_START) as usize] = 0x00;
        self.io[0x42] = 0x00;
        self.io[0x43] = 0x00;
        self.io[(LY_REGISTER - IO_START) as usize] = 0x00;
        self.io[0x45] = 0x00;
        self.io[0x47] = 0xFF;
        self.io[0x48] = 0xFF;
        self.io[0x49] = 0xFF;
        self.io[0x4A] = 0x00;
        self.io[0x4B] = 0x00;
        self.io[(DMA_REGISTER - IO_START) as usize] = 0xFF;
        self.io[0x10] = 0x00;
        self.io[0x11] = 0x00;
        self.io[0x12] = 0x00;
        self.io[0x13] = 0x00;
        self.io[0x14] = 0x00;
        self.io[0x16] = 0x00;
        self.io[0x17] = 0x00;
        self.io[0x18] = 0x00;
        self.io[0x19] = 0x00;
        self.io[0x1A] = 0x00;
        self.io[0x1B] = 0x00;
        self.io[0x1C] = 0x00;
        self.io[0x1D] = 0x00;
        self.io[0x1E] = 0x00;
        self.io[0x20] = 0x00;
        self.io[0x21] = 0x00;
        self.io[0x22] = 0x00;
        self.io[0x23] = 0x00;
        self.io[0x24] = 0x00;
        self.io[0x25] = 0x00;
        self.io[0x26] = 0x70;
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
            ppu_mode: self.ppu_mode.bits(),
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

    pub fn cartridge_title(&self) -> &str {
        self.cartridge.title()
    }

    pub fn cartridge_type_code(&self) -> u8 {
        self.cartridge.cartridge_type_code()
    }

    pub fn cartridge_has_battery(&self) -> bool {
        self.cartridge.has_battery()
    }

    pub fn cartridge_has_rtc(&self) -> bool {
        self.cartridge.has_rtc()
    }

    pub fn save_cartridge_state(&self) -> Result<Vec<u8>, GbError> {
        self.cartridge.save_persistent_state()
    }

    pub fn load_cartridge_state(&mut self, bytes: &[u8]) -> Result<(), GbError> {
        self.cartridge.load_persistent_state(bytes)
    }

    pub fn save_cartridge_ram(&self) -> Vec<u8> {
        self.cartridge.ram_data().to_vec()
    }

    pub fn load_cartridge_ram(&mut self, bytes: &[u8]) -> Result<(), GbError> {
        self.cartridge.load_ram_data(bytes)
    }

    pub fn pressed_buttons(&self) -> &'static [&'static str] {
        const EMPTY: &[&str] = &[];
        const RIGHT: &[&str] = &["right"];
        const LEFT: &[&str] = &["left"];
        const UP: &[&str] = &["up"];
        const DOWN: &[&str] = &["down"];
        const A: &[&str] = &["a"];
        const B: &[&str] = &["b"];
        const SELECT: &[&str] = &["select"];
        const START: &[&str] = &["start"];
        const MULTI: &[&str] = &[];

        match self.joypad.pressed.count_ones() {
            0 => EMPTY,
            1 => match self.joypad.pressed.trailing_zeros() {
                0 => RIGHT,
                1 => LEFT,
                2 => UP,
                3 => DOWN,
                4 => A,
                5 => B,
                6 => SELECT,
                7 => START,
                _ => EMPTY,
            },
            _ => MULTI,
        }
    }

    pub fn pressed_button_names(&self) -> Vec<&'static str> {
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
            if self.joypad.pressed & (1 << bit) != 0 {
                buttons.push(name);
            }
        }
        buttons
    }

    pub fn set_pressed_buttons_mask(&mut self, pressed: u8) {
        let previous = self.current_joypad_bits();
        self.joypad.pressed = pressed;
        let current = self.current_joypad_bits();
        if previous & !current != 0 {
            self.request_interrupt(0x10);
        }
    }

    pub fn read_system_address(&mut self, address: u16) -> u8 {
        self.read8(address)
    }

    pub fn write_system_address(&mut self, address: u16, value: u8) {
        self.write8(address, value);
    }

    fn model_boot_registers(model: GbModel) -> Registers {
        match model {
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

    fn model_boot_p1(model: GbModel) -> u8 {
        match model {
            GbModel::Dmg0 | GbModel::Dmg | GbModel::Mgb => 0xCF,
            GbModel::Sgb | GbModel::Sgb2 => 0xFF,
        }
    }

    fn model_boot_lcdc(model: GbModel) -> u8 {
        match model {
            GbModel::Dmg0 => 0x91,
            GbModel::Dmg | GbModel::Mgb => 0x91,
            GbModel::Sgb | GbModel::Sgb2 => 0x91,
        }
    }

    fn model_boot_stat(model: GbModel) -> u8 {
        match model {
            GbModel::Dmg0 => 0x83,
            GbModel::Dmg | GbModel::Mgb => 0x80,
            GbModel::Sgb | GbModel::Sgb2 => 0x80,
        }
    }

    fn model_boot_lyc(model: GbModel) -> u8 {
        match model {
            GbModel::Dmg0 => 0x00,
            GbModel::Dmg | GbModel::Mgb => 0x00,
            GbModel::Sgb | GbModel::Sgb2 => 0x00,
        }
    }

    fn model_boot_obp0(model: GbModel) -> u8 {
        match model {
            GbModel::Sgb | GbModel::Sgb2 => 0x00,
            _ => 0xFF,
        }
    }

    fn model_boot_nr52(model: GbModel) -> u8 {
        match model {
            GbModel::Sgb | GbModel::Sgb2 => 0xF0,
            _ => 0xF1,
        }
    }

    fn model_boot_div_counter(model: GbModel) -> u16 {
        match model {
            GbModel::Dmg0 => 0x0634,
            GbModel::Dmg => 0x2AF4,
            GbModel::Mgb => 0x2AF4,
            GbModel::Sgb | GbModel::Sgb2 => 0x3634,
        }
    }

    fn model_boot_lcdc_to_entry_cycles(model: GbModel) -> u16 {
        match model {
            GbModel::Dmg0 => 540,
            GbModel::Dmg | GbModel::Mgb | GbModel::Sgb | GbModel::Sgb2 => 256,
        }
    }

    fn synthetic_bootrom(model: GbModel) -> [u8; 0x100] {
        fn emit(code: &mut [u8; 0x100], pc: &mut usize, bytes: &[u8]) {
            let end = *pc + bytes.len();
            code[*pc..end].copy_from_slice(bytes);
            *pc = end;
        }

        fn emit_ldh_write(code: &mut [u8; 0x100], pc: &mut usize, offset: u8, value: u8) {
            emit(code, pc, &[0x3E, value, 0xE0, offset]);
        }

        fn emit_delay(code: &mut [u8; 0x100], pc: &mut usize, cycles: u16) {
            fn delay_call_cycles(outer: u8, inner: u8) -> u32 {
                16 * outer as u32 * inner as u32 + 12 * outer as u32 + 52
            }

            let mut best = None;
            for outer in 1..=u8::MAX {
                for inner in 1..=u8::MAX {
                    let delay = delay_call_cycles(outer, inner);
                    if delay > cycles as u32 {
                        continue;
                    }
                    let nops = ((cycles as u32) - delay) / 4;
                    let bytes = 7 + nops as usize;
                    match best {
                        Some((best_bytes, _, _, _)) if best_bytes <= bytes => {}
                        _ => best = Some((bytes, outer, inner, nops)),
                    }
                }
            }

            if let Some((_, outer, inner, nops)) = best {
                emit(code, pc, &[0x06, outer, 0x16, inner, 0xCD, 0x02, 0x00]);
                for _ in 0..nops {
                    emit(code, pc, &[0x00]);
                }
            } else {
                let nops = cycles / 4;
                for _ in 0..nops {
                    emit(code, pc, &[0x00]);
                }
            }
        }

        let mut code = [0x00; 0x100];
        let mut pc = 0usize;
        let target = Self::model_boot_registers(model);
        let boot_writes = [
            (0x00, Self::model_boot_p1(model)),
            (0x0F, 0xE1),
            (0x10, 0x80),
            (0x11, 0xBF),
            (0x12, 0xF3),
            (0x13, 0xFF),
            (0x14, 0xBF),
            (0x16, 0x3F),
            (0x17, 0x00),
            (0x18, 0xFF),
            (0x19, 0xBF),
            (0x1A, 0x7F),
            (0x1B, 0xFF),
            (0x1C, 0x9F),
            (0x1D, 0xFF),
            (0x1E, 0xBF),
            (0x20, 0xFF),
            (0x21, 0x00),
            (0x22, 0x00),
            (0x23, 0xBF),
            (0x24, 0x77),
            (0x25, 0xF3),
            (0x26, Self::model_boot_nr52(model)),
            (0x41, Self::model_boot_stat(model)),
            (0x45, Self::model_boot_lyc(model)),
            (0x47, 0xFC),
            (0x48, Self::model_boot_obp0(model)),
            (0x49, 0xFF),
            (0x4A, 0x00),
            (0x4B, 0x00),
            (0x40, Self::model_boot_lcdc(model)),
        ];
        let fixed_cycles = 12
            + (boot_writes.len() as u16 * 20)
            + 48
            + match target.f {
                0xB0 => 52,
                0x00 => 44,
                _ => unreachable!("unsupported synthetic boot flags"),
            };
        let startup_delay_cycles = Self::model_boot_div_counter(model)
            .saturating_sub(fixed_cycles + Self::model_boot_lcdc_to_entry_cycles(model));

        emit(&mut code, &mut pc, &[0x18, 0x08]);
        emit(&mut code, &mut pc, &[0x4A, 0x0D, 0x20, 0xFD, 0x05, 0x20, 0xF9, 0xC9]);

        emit_delay(&mut code, &mut pc, startup_delay_cycles);

        for (offset, value) in boot_writes {
            emit_ldh_write(&mut code, &mut pc, offset, value);
            if offset == 0x40 {
                emit_delay(
                    &mut code,
                    &mut pc,
                    Self::model_boot_lcdc_to_entry_cycles(model),
                );
            }
        }

        emit(&mut code, &mut pc, &[0x31, 0xFE, 0xFF]);
        emit(&mut code, &mut pc, &[0x01, target.c, target.b]);
        emit(&mut code, &mut pc, &[0x11, target.e, target.d]);
        emit(&mut code, &mut pc, &[0x21, target.l, target.h]);

        match target.f {
            0xB0 => emit(&mut code, &mut pc, &[0xAF, 0x37, 0xCB, 0x47]),
            0x00 => emit(&mut code, &mut pc, &[0xAF, 0x3C]),
            _ => unreachable!("unsupported synthetic boot flags"),
        }

        emit(&mut code, &mut pc, &[0x3E, target.a]);
        emit(&mut code, &mut pc, &[0xE0, 0x50]);
        emit(&mut code, &mut pc, &[0xC3, 0x00, 0x01]);

        while pc < code.len() {
            emit(&mut code, &mut pc, &[0x00]);
        }
        code
    }

    pub(crate) fn set_exec_state(&mut self, state: ExecState) {
        self.exec_state = state;
    }

    pub fn save_state(&self) -> Result<Vec<u8>, GbError> {
        let state = SnapshotState {
            cartridge: self.cartridge.clone(),
            bootrom: self.bootrom.as_ref().map(|data| data.to_vec()),
            bootrom_active: self.bootrom_active,
            model: self.model,
            joypad: self.joypad,
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
            halted: matches!(self.exec_state, ExecState::Halt | ExecState::Stop),
            halt_bug: false,
            instruction_counter: self.instruction_counter,
            cycle_counter: self.cycle_counter,
            div_counter: self.div_counter,
            tima_reload_state: self.tima_reload_state,
            ppu_cycle_counter: self.ppu_cycle_counter,
            ppu_mode: self.ppu_mode,
            ppu_mode_cycles_remaining: self.ppu_mode_cycles_remaining,
            dma_source: self.dma_source,
            dma_active: self.dma_active,
            dma_requested_source: self.dma_requested_source,
            dma_starting_source: self.dma_starting_source,
            dma_next_byte: self.dma_next_byte,
            pending_t34_interrupts: self.pending_t34_interrupts,
            pending_watchpoint: self.pending_watchpoint,
            deferred_interrupt_flags: 0,
            deferred_interrupt_delay: 0,
            trace: self
                .trace
                .iter()
                .copied()
                .map(SnapshotTraceEntry::from)
                .collect(),
            serial_output: self.serial_output.clone(),
        };

        serde_json::to_vec(&state).map_err(|_| GbError::StackOverflow(0))
    }

    pub fn load_state(bytes: &[u8]) -> Result<Self, GbError> {
        let state: SnapshotState =
            serde_json::from_slice(bytes).map_err(|_| GbError::StackOverflow(0))?;
        let mut machine = Self::new_with_model_and_bootrom(
            state.cartridge.rom_data().to_vec(),
            state.model,
            state.bootrom,
        )?;
        machine.cartridge = state.cartridge;
        machine.bootrom_active = state.bootrom_active;
        machine.joypad = state.joypad;
        machine.vram.copy_from_slice(&state.vram[..0x2000]);
        machine.wram.copy_from_slice(&state.wram[..0x2000]);
        machine.oam.copy_from_slice(&state.oam[..0xA0]);
        machine.io.copy_from_slice(&state.io[..0x80]);
        machine.hram.copy_from_slice(&state.hram[..0x7F]);
        machine.ie = state.ie;
        machine.registers = state.registers;
        machine.prefetched_pc = state.prefetched_pc;
        machine.prefetched_opcode = state.prefetched_opcode;
        machine.breakpoints = state
            .breakpoints
            .into_iter()
            .map(Breakpoint::from)
            .collect();
        machine.ime = state.ime;
        machine.exec_state = if state.halted {
            match state.exec_state {
                ExecState::Stop => ExecState::Stop,
                _ => ExecState::Halt,
            }
        } else {
            state.exec_state
        };
        machine.instruction_counter = state.instruction_counter;
        machine.cycle_counter = state.cycle_counter;
        machine.div_counter = state.div_counter;
        machine.tima_reload_state = state.tima_reload_state;
        machine.ppu_cycle_counter = state.ppu_cycle_counter;
        machine.ppu_mode = state.ppu_mode;
        machine.ppu_mode_cycles_remaining = state.ppu_mode_cycles_remaining;
        machine.dma_source = state.dma_source;
        machine.dma_active = state.dma_active;
        machine.dma_requested_source = state.dma_requested_source;
        machine.dma_starting_source = state.dma_starting_source;
        machine.dma_next_byte = state.dma_next_byte;
        machine.pending_t34_interrupts = state.pending_t34_interrupts;
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

    pub(crate) fn disassemble_one(&self, address: u16) -> DisassembledInstruction {
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
            0x10 => (2, "STOP".to_string()),
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
            0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD => {
                (1, format!("ILLEGAL ${opcode:02X}"))
            }
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

    pub(crate) fn push_trace(&mut self, entry: TraceEntry) {
        if self.trace.len() == TRACE_CAPACITY {
            self.trace.pop_front();
        }
        self.trace.push_back(entry);
    }

    pub(crate) fn has_pc_breakpoint(&self, pc: u16) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| matches!(bp, Breakpoint::ProgramCounter(value) if *value == u32::from(pc)))
    }

    pub(crate) fn has_opcode_breakpoint(&self, opcode: u8) -> bool {
        self.breakpoints
            .iter()
            .any(|bp| matches!(bp, Breakpoint::Opcode(value) if *value == opcode))
    }

    pub(crate) fn matching_watchpoint(&self, kind: WatchpointKind, address: u16) -> bool {
        self.breakpoints.iter().any(|bp| match (bp, kind) {
            (Breakpoint::MemoryRead(value), WatchpointKind::Read) => *value == u32::from(address),
            (Breakpoint::MemoryWrite(value), WatchpointKind::Write) => *value == u32::from(address),
            _ => false,
        })
    }

    pub(crate) fn record_watchpoint(&mut self, kind: WatchpointKind, address: u16) {
        if self.matching_watchpoint(kind, address) {
            self.pending_watchpoint = Some(WatchpointHit { kind, address });
        }
    }
}
