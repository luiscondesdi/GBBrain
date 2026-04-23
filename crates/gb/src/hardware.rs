use super::*;

impl GbMachine {
    pub(crate) fn current_joypad_bits(&self) -> u8 {
        let select = self.io[0x00] & 0x30;
        let mut bits = 0x0F;

        if select & 0x10 == 0 {
            if self.joypad.pressed & 0x01 != 0 {
                bits &= !0x01;
            }
            if self.joypad.pressed & 0x02 != 0 {
                bits &= !0x02;
            }
            if self.joypad.pressed & 0x04 != 0 {
                bits &= !0x04;
            }
            if self.joypad.pressed & 0x08 != 0 {
                bits &= !0x08;
            }
        }

        if select & 0x20 == 0 {
            if self.joypad.pressed & 0x10 != 0 {
                bits &= !0x01;
            }
            if self.joypad.pressed & 0x20 != 0 {
                bits &= !0x02;
            }
            if self.joypad.pressed & 0x40 != 0 {
                bits &= !0x04;
            }
            if self.joypad.pressed & 0x80 != 0 {
                bits &= !0x08;
            }
        }

        bits
    }

    pub(crate) fn apu_enabled(&self) -> bool {
        self.io[(0xFF26 - IO_START) as usize] & 0x80 != 0
    }

    pub(crate) fn set_apu_channel_status(&mut self, mask: u8, enabled: bool) {
        let index = (0xFF26 - IO_START) as usize;
        let current = self.io[index];
        let next = if enabled {
            current | mask
        } else {
            current & !mask
        };
        self.io[index] = (next & 0x8F) | 0x70;
    }

    pub(crate) fn read_io_value(&self, address: u16) -> u8 {
        let raw = self.io[(address - IO_START) as usize];
        match address {
            0xFF00 => 0xC0 | (raw & 0x30) | self.current_joypad_bits(),
            0xFF41 if !self.lcd_enabled() => 0x80,
            0xFF10 => raw | 0x80,
            0xFF11 => raw | 0x3F,
            0xFF13 => 0xFF,
            0xFF14 => raw | 0xBF,
            0xFF16 => raw | 0x3F,
            0xFF18 => 0xFF,
            0xFF19 => raw | 0xBF,
            0xFF1A => raw | 0x7F,
            0xFF1B => 0xFF,
            0xFF1C => raw | 0x9F,
            0xFF1D => 0xFF,
            0xFF1E => raw | 0xBF,
            0xFF20 => 0xFF,
            0xFF23 => raw | 0xBF,
            _ => raw,
        }
    }

    pub(crate) fn read_io_value_cycle(&mut self, address: u16) -> u8 {
        match address {
            TIMER_DIV | TIMER_TIMA | TIMER_TMA | TIMER_TAC => {
                self.step_timer_cycle();
                self.read_io_value(address)
            }
            _ => self.read_io_value(address),
        }
    }

    pub(crate) fn cpu_bus(address: u16) -> Bus {
        match address {
            0x0000..=ROM_BANK_N_END
            | 0xA000..=0xBFFF
            | WRAM_START..=WRAM_END
            | ECHO_START..=ECHO_END => Bus::External,
            VRAM_START..=VRAM_END => Bus::ExternalVideo,
            OAM_START..=OAM_END => Bus::InternalVideo,
            _ => Bus::Internal,
        }
    }

    pub(crate) fn dma_source_bus(&self) -> Option<Bus> {
        if !self.dma_active() {
            return None;
        }
        let high = (self.dma_source >> 8) as u8;
        Some(match high {
            0x80..=0x9F => Bus::ExternalVideo,
            _ => Bus::External,
        })
    }

    pub(crate) fn dma_blocks_cpu_access(&self, address: u16) -> bool {
        let cpu_bus = Self::cpu_bus(address);
        if cpu_bus == Bus::InternalVideo {
            return self.dma_active();
        }
        matches!(self.dma_source_bus(), Some(source_bus) if source_bus == cpu_bus)
    }

    pub(crate) fn lcd_enabled(&self) -> bool {
        self.io[(LCDC_REGISTER - IO_START) as usize] & 0x80 != 0
    }

    pub(crate) fn ppu_mode(&self) -> u8 {
        if !self.lcd_enabled() {
            return 0;
        }
        self.ppu_mode.bits()
    }

    pub(crate) fn refresh_stat(&mut self) {
        if !self.lcd_enabled() {
            return;
        }
        let stat_index = (STAT_REGISTER - IO_START) as usize;
        let mut stat = (self.io[stat_index] & 0xFC) | 0x80;
        stat |= self.ppu_mode() & 0x03;
        self.io[stat_index] = stat;
    }

    pub(crate) fn set_stat_lyc_flag(&mut self, matches: bool) {
        let stat_index = (STAT_REGISTER - IO_START) as usize;
        if matches {
            self.io[stat_index] |= 0x04;
        } else {
            self.io[stat_index] &= !0x04;
        }
    }

    pub(crate) fn oam_bug_row(&self) -> Option<usize> {
        if self.ppu_mode() == 2 {
            Some(usize::from((self.ppu_cycle_counter / 4).min(19)))
        } else {
            None
        }
    }

    pub(crate) fn oam_bug_applies(address: u16) -> bool {
        (0xFE00..=0xFEFF).contains(&address)
    }

    pub(crate) fn oam_word(&self, row: usize, word: usize) -> u16 {
        let base = row * 8 + word * 2;
        u16::from_le_bytes([self.oam[base], self.oam[base + 1]])
    }

    pub(crate) fn set_oam_word(&mut self, row: usize, word: usize, value: u16) {
        let base = row * 8 + word * 2;
        let [lo, hi] = value.to_le_bytes();
        self.oam[base] = lo;
        self.oam[base + 1] = hi;
    }

    pub(crate) fn apply_oam_row_corruption(&mut self, row: usize, kind: OamCorruptionKind) {
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
        }
    }

    pub(crate) fn maybe_trigger_oam_bug(&mut self, address: u16, kind: OamCorruptionKind) {
        if !Self::oam_bug_applies(address) {
            return;
        }
        if let Some(row) = self.oam_bug_row() {
            self.apply_oam_row_corruption(row, kind);
        }
    }

    pub(crate) fn read8_with_kind(&mut self, address: u16, _kind: OamCorruptionKind) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
            return 0xFF;
        }
        if self.ppu_mode() == 3 && matches!(address, VRAM_START..=VRAM_END | OAM_START..=OAM_END) {
            return 0xFF;
        }
        if matches!(address, TIMER_DIV | TIMER_TIMA | TIMER_TMA | TIMER_TAC) {
            return self.read_io_value_cycle(address);
        }
        if self.dma_blocks_cpu_access(address) {
            return 0xFF;
        }
        self.peek8(address)
    }

    pub(crate) fn read8(&mut self, address: u16) -> u8 {
        self.read8_with_kind(address, OamCorruptionKind::Read)
    }

    pub(crate) fn is_unmapped_io(address: u16) -> bool {
        matches!(
            address,
            0xFF03
                | 0xFF08..=0xFF0E
                | 0xFF15
                | 0xFF1F
                | 0xFF27..=0xFF29
                | 0xFF4C..=0xFF4F
                | 0xFF51..=0xFF7F
        )
    }

    pub(crate) fn peek8(&self, address: u16) -> u8 {
        if self.dma_blocks_cpu_access(address) {
            return 0xFF;
        }
        match address {
            0x0000..=0x00FF if self.bootrom_active => self
                .bootrom
                .as_ref()
                .map(|data| data[address as usize])
                .unwrap_or(0xFF),
            0x0000..=ROM_BANK_0_END => self.cartridge.read_lower_rom(address),
            0x4000..=ROM_BANK_N_END => self.cartridge.read_upper_rom(address),
            0xA000..=0xBFFF => self.cartridge.read_ram(address),
            VRAM_START..=VRAM_END => self.vram[(address - VRAM_START) as usize],
            WRAM_START..=WRAM_END => self.wram[(address - WRAM_START) as usize],
            ECHO_START..=ECHO_END => self.wram[(address - ECHO_START) as usize],
            OAM_START..=OAM_END => self.oam[(address - OAM_START) as usize],
            IO_START..=IO_END => {
                if Self::is_unmapped_io(address) {
                    0xFF
                } else {
                    self.read_io_value(address)
                }
            }
            HRAM_START..=HRAM_END => self.hram[(address - HRAM_START) as usize],
            IE_REGISTER => self.ie,
            _ => 0xFF,
        }
    }

    pub(crate) fn read8_without_oam_bug(&mut self, address: u16) -> u8 {
        self.record_watchpoint(WatchpointKind::Read, address);
        if Self::oam_bug_applies(address) && self.ppu_mode() == 2 {
            return 0xFF;
        }
        if self.ppu_mode() == 3 && matches!(address, VRAM_START..=VRAM_END | OAM_START..=OAM_END) {
            return 0xFF;
        }
        self.peek8(address)
    }

    pub(crate) fn write8(&mut self, address: u16, value: u8) {
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
            0x0000..=0x7FFF => self.cartridge.write_control(address, value),
            0xA000..=0xBFFF => self.cartridge.write_ram(address, value),
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

    pub(crate) fn dma_active(&self) -> bool {
        self.dma_active
    }

    pub(crate) fn step_dma_mcycle(&mut self) {
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

    pub(crate) fn dma_source_byte(&self, address: u16) -> u8 {
        match address {
            0x0000..=0x00FF if self.bootrom_active => self
                .bootrom
                .as_ref()
                .map(|data| data[address as usize])
                .unwrap_or(0xFF),
            0x0000..=ROM_BANK_0_END => self.cartridge.read_lower_rom(address),
            0x4000..=ROM_BANK_N_END => self.cartridge.read_upper_rom(address),
            0x8000..=0x9FFF => self.vram[(address - 0x8000) as usize],
            0xA000..=0xBFFF => self.cartridge.read_ram(address),
            0xC000..=0xDFFF => self.wram[(address - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(address - 0xE000) as usize],
            0xFE00..=0xFFFF => {
                let external_address = address.wrapping_sub(0x2000);
                self.dma_source_byte(external_address)
            }
        }
    }

    pub(crate) fn write_io(&mut self, address: u16, value: u8) {
        if Self::is_unmapped_io(address) {
            return;
        }

        match address {
            IO_START => {
                let previous = self.current_joypad_bits();
                self.io[(address - IO_START) as usize] = 0xC0 | (value & 0x30) | 0x0F;
                let current = self.current_joypad_bits();
                if previous & !current != 0 {
                    self.request_interrupt(0x10);
                }
            }
            LCDC_REGISTER => {
                let was_enabled = self.lcd_enabled();
                self.io[(address - IO_START) as usize] = value;
                let now_enabled = value & 0x80 != 0;
                if !was_enabled && now_enabled {
                    self.ppu_mode = PpuMode::HBlank;
                    self.ppu_mode_cycles_remaining = self.ppu_mode_duration(PpuMode::AccessOam);
                    self.set_stat_lyc_flag(true);
                    self.ppu_cycle_counter = 0;
                } else if was_enabled && !now_enabled {
                    self.ppu_mode = PpuMode::AccessOam;
                    self.ppu_mode_cycles_remaining = PPU_ACCESS_OAM_CYCLES;
                    self.io[(LY_REGISTER - IO_START) as usize] = 0;
                    self.ppu_cycle_counter = 0;
                }
                self.refresh_stat();
            }
            STAT_REGISTER => {
                let index = (address - IO_START) as usize;
                let preserved = self.io[index] & 0x04;
                self.io[index] = preserved | (value & 0x78);
                self.refresh_stat();
            }
            LY_REGISTER => {
                let old_match =
                    self.io[(LY_REGISTER - IO_START) as usize] == self.io[(0xFF45 - IO_START) as usize];
                self.io[(address - IO_START) as usize] = 0;
                let new_match =
                    self.io[(LY_REGISTER - IO_START) as usize] == self.io[(0xFF45 - IO_START) as usize];
                self.set_stat_lyc_flag(new_match);
                if new_match && !old_match && (self.io[(STAT_REGISTER - IO_START) as usize] & 0x40 != 0) {
                    self.request_interrupt(0x02);
                }
            }
            0xFF45 => {
                self.io[(address - IO_START) as usize] = value;
            }
            TIMER_DIV => {
                self.step_timer_cycle();
                let old_signal = self.timer_signal();
                self.div_counter = 0;
                self.io[(address - IO_START) as usize] = 0;
                if old_signal && !self.timer_signal() {
                    self.increment_tima();
                }
            }
            TIMER_TIMA => {
                self.step_timer_cycle();
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
                self.step_timer_cycle();
                self.io[(address - IO_START) as usize] = value;
                if matches!(
                    self.tima_reload_state,
                    Some(TimaReloadState::ReloadWindow(_))
                ) {
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
            0xFF50 => {
                if self.bootrom_active && value & 0x01 != 0 {
                    self.bootrom_active = false;
                }
            }
            TIMER_TAC => {
                self.step_timer_cycle();
                let old_signal = self.timer_signal();
                self.io[(address - IO_START) as usize] = value | 0xF8;
                if old_signal && !self.timer_signal() {
                    self.increment_tima();
                }
            }
            SERIAL_SC => {
                self.io[(address - IO_START) as usize] = value | 0x7E;
                if value == 0x81 {
                    let byte = self.io[(SERIAL_SB - IO_START) as usize];
                    self.serial_output.push(byte);
                    self.io[(SERIAL_SC - IO_START) as usize] = 0x7F;
                    self.request_interrupt(0x08);
                }
            }
            IF_REGISTER => {
                self.io[(address - IO_START) as usize] = value | 0xE0;
            }
            0xFF10 => {
                self.io[(address - IO_START) as usize] = value | 0x80;
            }
            0xFF1A => {
                self.io[(address - IO_START) as usize] = (value & 0x80) | 0x7F;
            }
            0xFF1C => {
                self.io[(address - IO_START) as usize] = (value & 0x60) | 0x9F;
            }
            0xFF20 => {
                self.io[(address - IO_START) as usize] = value | 0xC0;
            }
            0xFF26 => {
                let low_status = if value & 0x80 != 0 {
                    self.io[(address - IO_START) as usize] & 0x0F
                } else {
                    0
                };
                self.io[(address - IO_START) as usize] = (value & 0x80) | 0x70 | low_status;
            }
            0xFF14 => {
                self.io[(address - IO_START) as usize] = (value & 0xC0) | 0x07;
                if self.apu_enabled() && value & 0x80 != 0 {
                    self.set_apu_channel_status(0x01, true);
                }
            }
            0xFF19 => {
                self.io[(address - IO_START) as usize] = (value & 0xC0) | 0x3F;
                if self.apu_enabled() && value & 0x80 != 0 {
                    self.set_apu_channel_status(0x02, true);
                }
            }
            0xFF1E => {
                self.io[(address - IO_START) as usize] = (value & 0xC0) | 0x3F;
                if self.apu_enabled() && value & 0x80 != 0 {
                    self.set_apu_channel_status(0x04, true);
                }
            }
            0xFF23 => {
                self.io[(address - IO_START) as usize] = (value & 0xC0) | 0x3F;
                if self.apu_enabled() && value & 0x80 != 0 {
                    self.set_apu_channel_status(0x08, true);
                }
            }
            _ => {
                self.io[(address - IO_START) as usize] = value;
            }
        }
    }
}
