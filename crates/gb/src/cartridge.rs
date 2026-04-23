use serde::{Deserialize, Serialize};

use super::GbError;

const ROM_BANK_SIZE: usize = 0x4000;
const RAM_BANK_SIZE: usize = 0x2000;
const RTC_CYCLES_PER_SECOND: u64 = 4_194_304;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistentState {
    rom_signature: Vec<u8>,
    ram: Vec<u8>,
    rtc: Option<Mbc3Rtc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CartridgeType {
    NoMbc {
        ram: bool,
        battery: bool,
    },
    Mbc1 {
        ram: bool,
        battery: bool,
        multicart: bool,
    },
    Mbc2 {
        battery: bool,
    },
    Mbc3 {
        ram: bool,
        battery: bool,
        rtc: bool,
    },
    Mbc5 {
        ram: bool,
        battery: bool,
        rumble: bool,
    },
    Huc1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CartridgeRomSize {
    Banks2,
    Banks4,
    Banks8,
    Banks16,
    Banks32,
    Banks64,
    Banks128,
    Banks256,
    Banks512,
    Banks72,
    Banks80,
    Banks96,
}

impl CartridgeRomSize {
    fn from_header(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Banks2),
            0x01 => Some(Self::Banks4),
            0x02 => Some(Self::Banks8),
            0x03 => Some(Self::Banks16),
            0x04 => Some(Self::Banks32),
            0x05 => Some(Self::Banks64),
            0x06 => Some(Self::Banks128),
            0x07 => Some(Self::Banks256),
            0x08 => Some(Self::Banks512),
            0x52 => Some(Self::Banks72),
            0x53 => Some(Self::Banks80),
            0x54 => Some(Self::Banks96),
            _ => None,
        }
    }

    fn bytes(self) -> usize {
        self.banks() * ROM_BANK_SIZE
    }

    fn banks(self) -> usize {
        match self {
            Self::Banks2 => 2,
            Self::Banks4 => 4,
            Self::Banks8 => 8,
            Self::Banks16 => 16,
            Self::Banks32 => 32,
            Self::Banks64 => 64,
            Self::Banks128 => 128,
            Self::Banks256 => 256,
            Self::Banks512 => 512,
            Self::Banks72 => 72,
            Self::Banks80 => 80,
            Self::Banks96 => 96,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CartridgeRamSize {
    None,
    Ram2K,
    Ram8K,
    Ram32K,
    Ram128K,
    Ram64K,
}

impl CartridgeRamSize {
    fn from_header(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::None),
            0x01 => Some(Self::Ram2K),
            0x02 => Some(Self::Ram8K),
            0x03 => Some(Self::Ram32K),
            0x04 => Some(Self::Ram128K),
            0x05 => Some(Self::Ram64K),
            _ => None,
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::None => 0,
            Self::Ram2K => 2048,
            Self::Ram8K => 8192,
            Self::Ram32K => 32768,
            Self::Ram128K => 131_072,
            Self::Ram64K => 65536,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Mbc1State {
    ram_enabled: bool,
    bank1: u8,
    bank2: u8,
    mode: bool,
}

impl Default for Mbc1State {
    fn default() -> Self {
        Self {
            ram_enabled: false,
            bank1: 1,
            bank2: 0,
            mode: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Mbc2State {
    ram_enabled: bool,
    rom_bank: u8,
}

impl Default for Mbc2State {
    fn default() -> Self {
        Self {
            ram_enabled: false,
            rom_bank: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Mbc3State {
    ram_rtc_enabled: bool,
    rom_bank: u8,
    map_select: u8,
    latch_value: u8,
}

impl Default for Mbc3State {
    fn default() -> Self {
        Self {
            ram_rtc_enabled: false,
            rom_bank: 1,
            map_select: 0,
            latch_value: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Mbc5State {
    ram_enabled: bool,
    romb0: u8,
    romb1: u8,
    ramb: u8,
}

impl Default for Mbc5State {
    fn default() -> Self {
        Self {
            ram_enabled: false,
            romb0: 1,
            romb1: 0,
            ramb: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Huc1State {
    mode: u8,
    rom_bank: u8,
    ram_bank: u8,
}

impl Default for Huc1State {
    fn default() -> Self {
        Self {
            mode: 0,
            rom_bank: 0,
            ram_bank: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum Mapper {
    None,
    Mbc1(Mbc1State),
    Mbc2(Mbc2State),
    Mbc3(Mbc3State),
    Mbc5(Mbc5State),
    Huc1(Huc1State),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RtcRegisters {
    seconds: u8,
    minutes: u8,
    hours: u8,
    day_low: u8,
    day_high: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Mbc3Rtc {
    live: RtcRegisters,
    latched: RtcRegisters,
    cycle_accumulator: u64,
}

impl Default for Mbc3Rtc {
    fn default() -> Self {
        let regs = RtcRegisters {
            seconds: 0,
            minutes: 0,
            hours: 0,
            day_low: 0,
            day_high: 0,
        };
        Self {
            live: regs,
            latched: regs,
            cycle_accumulator: 0,
        }
    }
}

impl Mbc3Rtc {
    fn latched_value(&self, select: u8) -> u8 {
        match select {
            0x08 => self.latched.seconds,
            0x09 => self.latched.minutes,
            0x0A => self.latched.hours,
            0x0B => self.latched.day_low,
            0x0C => self.latched.day_high,
            _ => 0xFF,
        }
    }

    fn write_live(&mut self, select: u8, value: u8) {
        match select {
            0x08 => self.live.seconds = value % 60,
            0x09 => self.live.minutes = value % 60,
            0x0A => self.live.hours = value % 24,
            0x0B => self.live.day_low = value,
            0x0C => self.live.day_high = value & 0xC1,
            _ => {}
        }
    }

    fn latch(&mut self) {
        self.latched = self.live;
    }

    fn halted(&self) -> bool {
        self.live.day_high & 0x40 != 0
    }

    fn tick(&mut self, cycles: u16) {
        if self.halted() {
            return;
        }

        self.cycle_accumulator += u64::from(cycles);
        while self.cycle_accumulator >= RTC_CYCLES_PER_SECOND {
            self.cycle_accumulator -= RTC_CYCLES_PER_SECOND;
            self.increment_second();
        }
    }

    fn increment_second(&mut self) {
        self.live.seconds = self.live.seconds.wrapping_add(1);
        if self.live.seconds < 60 {
            return;
        }
        self.live.seconds = 0;
        self.live.minutes = self.live.minutes.wrapping_add(1);
        if self.live.minutes < 60 {
            return;
        }
        self.live.minutes = 0;
        self.live.hours = self.live.hours.wrapping_add(1);
        if self.live.hours < 24 {
            return;
        }
        self.live.hours = 0;

        let day = u16::from(self.live.day_low) | (u16::from(self.live.day_high & 0x01) << 8);
        let next_day = day.wrapping_add(1);
        self.live.day_low = next_day as u8;
        self.live.day_high = (self.live.day_high & !0x01) | (((next_day >> 8) as u8) & 0x01);
        if day == 0x01FF {
            self.live.day_high |= 0x80;
            self.live.day_low = 0;
            self.live.day_high &= !0x01;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Cartridge {
    rom: Vec<u8>,
    ram: Vec<u8>,
    title: String,
    cartridge_type_code: u8,
    kind: CartridgeType,
    ram_size: CartridgeRamSize,
    mapper: Mapper,
    rom_offsets: (usize, usize),
    ram_offset: usize,
    rtc: Option<Mbc3Rtc>,
}

impl Cartridge {
    pub(crate) fn new(rom: Vec<u8>) -> Result<Self, GbError> {
        if rom.is_empty() {
            return Err(GbError::EmptyRom);
        }
        if rom.len() < 0x8000 || rom.len() % ROM_BANK_SIZE != 0 {
            return Err(GbError::InvalidRomLength(rom.len()));
        }

        let header_offset = 0;
        let cartridge_type_code = rom[header_offset + 0x147];
        let kind = CartridgeType::from_header(cartridge_type_code, &rom)?;
        let title = parse_title(&rom, header_offset)?;
        let rom_size = CartridgeRomSize::from_header(rom[header_offset + 0x148])
            .ok_or(GbError::UnsupportedRomSize(rom[header_offset + 0x148]))?;
        let ram_size = CartridgeRamSize::from_header(rom[header_offset + 0x149])
            .ok_or(GbError::UnsupportedRamSize(rom[header_offset + 0x149]))?;

        if rom_size.bytes() != rom.len() {
            return Err(GbError::RomSizeMismatch {
                header_size: rom_size.bytes(),
                actual_size: rom.len(),
            });
        }
        if kind.has_external_ram() && matches!(ram_size, CartridgeRamSize::None) {
            return Err(GbError::MissingRamForCartridge(cartridge_type_code));
        }
        if !kind.has_external_ram()
            && !matches!(kind, CartridgeType::Mbc2 { .. })
            && !matches!(ram_size, CartridgeRamSize::None)
        {
            return Err(GbError::UnexpectedRamSize {
                cartridge_type: cartridge_type_code,
                ram_size: rom[header_offset + 0x149],
            });
        }

        let mapper = match kind {
            CartridgeType::NoMbc { .. } => Mapper::None,
            CartridgeType::Mbc1 { .. } => Mapper::Mbc1(Mbc1State::default()),
            CartridgeType::Mbc2 { .. } => Mapper::Mbc2(Mbc2State::default()),
            CartridgeType::Mbc3 { .. } => Mapper::Mbc3(Mbc3State::default()),
            CartridgeType::Mbc5 { .. } => Mapper::Mbc5(Mbc5State::default()),
            CartridgeType::Huc1 => Mapper::Huc1(Huc1State::default()),
        };

        Ok(Self {
            rom,
            ram: vec![
                0;
                match kind {
                    CartridgeType::Mbc2 { .. } => 512,
                    _ => ram_size.bytes(),
                }
            ],
            title,
            cartridge_type_code,
            kind,
            ram_size,
            mapper,
            rom_offsets: (0, ROM_BANK_SIZE),
            ram_offset: 0,
            rtc: match kind {
                CartridgeType::Mbc3 { rtc: true, .. } => Some(Mbc3Rtc::default()),
                _ => None,
            },
        })
    }

    pub(crate) fn reset(&mut self) {
        self.mapper = match self.kind {
            CartridgeType::NoMbc { .. } => Mapper::None,
            CartridgeType::Mbc1 { .. } => Mapper::Mbc1(Mbc1State::default()),
            CartridgeType::Mbc2 { .. } => Mapper::Mbc2(Mbc2State::default()),
            CartridgeType::Mbc3 { .. } => Mapper::Mbc3(Mbc3State::default()),
            CartridgeType::Mbc5 { .. } => Mapper::Mbc5(Mbc5State::default()),
            CartridgeType::Huc1 => Mapper::Huc1(Huc1State::default()),
        };
        self.rom_offsets = (0, ROM_BANK_SIZE);
        self.ram_offset = 0;
    }

    pub(crate) fn rom_data(&self) -> &[u8] {
        &self.rom
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn cartridge_type_code(&self) -> u8 {
        self.cartridge_type_code
    }

    pub(crate) fn has_battery(&self) -> bool {
        self.kind.has_battery()
    }

    pub(crate) fn has_rtc(&self) -> bool {
        matches!(self.kind, CartridgeType::Mbc3 { rtc: true, .. })
    }

    pub(crate) fn ram_data(&self) -> &[u8] {
        &self.ram
    }

    pub(crate) fn load_ram_data(&mut self, bytes: &[u8]) -> Result<(), GbError> {
        if bytes.len() != self.ram.len() {
            return Err(GbError::PersistentStateRamSizeMismatch {
                expected: self.ram.len(),
                actual: bytes.len(),
            });
        }
        self.ram.copy_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn save_persistent_state(&self) -> Result<Vec<u8>, GbError> {
        serde_json::to_vec(&PersistentState {
            rom_signature: self.rom_signature().to_vec(),
            ram: self.ram.clone(),
            rtc: self.rtc.clone(),
        })
        .map_err(|_| GbError::StackOverflow(0))
    }

    pub(crate) fn load_persistent_state(&mut self, bytes: &[u8]) -> Result<(), GbError> {
        let state: PersistentState =
            serde_json::from_slice(bytes).map_err(|_| GbError::StackOverflow(0))?;
        if state.rom_signature != self.rom_signature() {
            return Err(GbError::PersistentStateCartridgeMismatch);
        }
        if state.ram.len() != self.ram.len() {
            return Err(GbError::PersistentStateRamSizeMismatch {
                expected: self.ram.len(),
                actual: state.ram.len(),
            });
        }
        if state.rtc.is_some() != self.rtc.is_some() {
            return Err(GbError::PersistentStateRtcMismatch);
        }
        self.ram = state.ram;
        self.rtc = state.rtc;
        Ok(())
    }

    pub(crate) fn read_lower_rom(&self, addr: u16) -> u8 {
        let (lower, _) = self.rom_offsets;
        self.rom[(lower + (addr as usize & 0x3FFF)) % self.rom.len()]
    }

    pub(crate) fn read_upper_rom(&self, addr: u16) -> u8 {
        let (_, upper) = self.rom_offsets;
        self.rom[(upper + (addr as usize & 0x3FFF)) % self.rom.len()]
    }

    pub(crate) fn read_ram(&self, addr: u16) -> u8 {
        match (&self.mapper, &self.rtc) {
            (Mapper::Mbc1(state), _) if state.ram_enabled => self.read_ram_bank(addr),
            (Mapper::Mbc2(state), _) if state.ram_enabled => {
                0xF0 | (self.read_ram_bank(addr) & 0x0F)
            }
            (Mapper::Mbc3(state), Some(rtc)) if state.ram_rtc_enabled => match state.map_select {
                0x00..=0x03 => self.read_ram_bank(addr),
                0x04..=0x07
                    if matches!(
                        self.ram_size,
                        CartridgeRamSize::Ram64K | CartridgeRamSize::Ram128K
                    ) =>
                {
                    self.read_ram_bank(addr)
                }
                0x08..=0x0C => rtc.latched_value(state.map_select),
                _ => 0xFF,
            },
            (Mapper::Mbc3(state), None) if state.ram_rtc_enabled && state.map_select <= 0x03 => {
                self.read_ram_bank(addr)
            }
            (Mapper::Mbc5(state), _) if state.ram_enabled => self.read_ram_bank(addr),
            (Mapper::Huc1(state), _) if matches!(state.mode, 0x00 | 0x0A) => {
                self.read_ram_bank(addr)
            }
            (Mapper::None, _) => self.read_ram_bank(addr),
            _ => 0xFF,
        }
    }

    pub(crate) fn write_ram(&mut self, addr: u16, value: u8) {
        match (&self.mapper, &mut self.rtc) {
            (Mapper::Mbc1(state), _) if state.ram_enabled => self.write_ram_bank(addr, value),
            (Mapper::Mbc2(state), _) if state.ram_enabled => {
                self.write_ram_bank(addr, value & 0x0F)
            }
            (Mapper::Mbc3(state), Some(rtc)) if state.ram_rtc_enabled => match state.map_select {
                0x00..=0x03 => self.write_ram_bank(addr, value),
                0x04..=0x07
                    if matches!(
                        self.ram_size,
                        CartridgeRamSize::Ram64K | CartridgeRamSize::Ram128K
                    ) =>
                {
                    self.write_ram_bank(addr, value)
                }
                0x08..=0x0C => rtc.write_live(state.map_select, value),
                _ => {}
            },
            (Mapper::Mbc3(state), None) if state.ram_rtc_enabled && state.map_select <= 0x03 => {
                self.write_ram_bank(addr, value)
            }
            (Mapper::Mbc5(state), _) if state.ram_enabled => self.write_ram_bank(addr, value),
            (Mapper::Huc1(state), _) if state.mode == 0x0A => self.write_ram_bank(addr, value),
            (Mapper::None, _) => self.write_ram_bank(addr, value),
            _ => {}
        }
    }

    pub(crate) fn write_control(&mut self, addr: u16, value: u8) {
        enum PostWrite {
            None,
            Mbc1(Mbc1State),
            Mbc5(Mbc5State),
            Huc1(Huc1State),
        }

        let mut post_write = PostWrite::None;
        match &mut self.mapper {
            Mapper::None => {}
            Mapper::Mbc1(state) => match addr {
                0x0000..=0x1FFF => state.ram_enabled = (value & 0x0F) == 0x0A,
                0x2000..=0x3FFF => {
                    state.bank1 = (value & 0x1F).max(1);
                    post_write = PostWrite::Mbc1(*state);
                }
                0x4000..=0x5FFF => {
                    state.bank2 = value & 0x03;
                    post_write = PostWrite::Mbc1(*state);
                }
                0x6000..=0x7FFF => {
                    state.mode = value & 0x01 != 0;
                    post_write = PostWrite::Mbc1(*state);
                }
                _ => {}
            },
            Mapper::Mbc2(state) => match addr {
                0x0000..=0x3FFF if addr & 0x0100 == 0 => state.ram_enabled = (value & 0x0F) == 0x0A,
                0x0000..=0x3FFF => {
                    state.rom_bank = (value & 0x0F).max(1);
                    self.rom_offsets = (0, ROM_BANK_SIZE * state.rom_bank as usize);
                }
                _ => {}
            },
            Mapper::Mbc3(state) => match addr {
                0x0000..=0x1FFF => state.ram_rtc_enabled = (value & 0x0F) == 0x0A,
                0x2000..=0x3FFF => {
                    state.rom_bank = if value & 0x7F == 0 { 1 } else { value & 0x7F };
                    self.rom_offsets = (0, ROM_BANK_SIZE * state.rom_bank as usize);
                }
                0x4000..=0x5FFF => {
                    state.map_select = value & 0x0F;
                    self.ram_offset = RAM_BANK_SIZE
                        * usize::from(match self.ram_size {
                            CartridgeRamSize::Ram64K | CartridgeRamSize::Ram128K => {
                                state.map_select & 0x07
                            }
                            _ => state.map_select & 0x03,
                        });
                }
                0x6000..=0x7FFF => {
                    if state.latch_value == 0 && value == 1 {
                        if let Some(rtc) = &mut self.rtc {
                            rtc.latch();
                        }
                    }
                    state.latch_value = value;
                }
                _ => {}
            },
            Mapper::Mbc5(state) => match addr {
                0x0000..=0x1FFF => state.ram_enabled = value == 0x0A,
                0x2000..=0x2FFF => {
                    state.romb0 = value;
                    post_write = PostWrite::Mbc5(*state);
                }
                0x3000..=0x3FFF => {
                    state.romb1 = value & 0x01;
                    post_write = PostWrite::Mbc5(*state);
                }
                0x4000..=0x5FFF => {
                    state.ramb = value & 0x0F;
                    self.ram_offset = RAM_BANK_SIZE * usize::from(state.ramb);
                }
                _ => {}
            },
            Mapper::Huc1(state) => match addr {
                0x0000..=0x1FFF => state.mode = value & 0x0F,
                0x2000..=0x3FFF => {
                    state.rom_bank = value & 0x3F;
                    post_write = PostWrite::Huc1(*state);
                }
                0x4000..=0x5FFF => {
                    state.ram_bank = value & 0x03;
                    post_write = PostWrite::Huc1(*state);
                }
                _ => {}
            },
        }

        match post_write {
            PostWrite::None => {}
            PostWrite::Mbc1(state) => self.recompute_mbc1_offsets(state),
            PostWrite::Mbc5(state) => self.recompute_mbc5_offsets(state),
            PostWrite::Huc1(state) => self.recompute_huc1_offsets(state),
        }
    }

    pub(crate) fn tick(&mut self, cycles: u16) {
        if let Some(rtc) = &mut self.rtc {
            rtc.tick(cycles);
        }
    }

    fn read_ram_bank(&self, addr: u16) -> u8 {
        if self.ram.is_empty() {
            return 0xFF;
        }
        let index = (self.ram_offset | (addr as usize & 0x1FFF)) & (self.ram.len() - 1);
        self.ram[index]
    }

    fn write_ram_bank(&mut self, addr: u16, value: u8) {
        if self.ram.is_empty() {
            return;
        }
        let index = (self.ram_offset | (addr as usize & 0x1FFF)) & (self.ram.len() - 1);
        self.ram[index] = value;
    }

    fn recompute_mbc1_offsets(&mut self, state: Mbc1State) {
        let (upper_bits, lower_bits) = match self.kind {
            CartridgeType::Mbc1 {
                multicart: true, ..
            } => (state.bank2 << 4, state.bank1 & 0x0F),
            _ => (state.bank2 << 5, state.bank1),
        };
        let lower_bank = if state.mode { upper_bits as usize } else { 0 };
        let upper_bank = usize::from(upper_bits | lower_bits);
        self.rom_offsets = (ROM_BANK_SIZE * lower_bank, ROM_BANK_SIZE * upper_bank);
        self.ram_offset = if state.mode {
            RAM_BANK_SIZE * usize::from(state.bank2)
        } else {
            0
        };
    }

    fn recompute_mbc5_offsets(&mut self, state: Mbc5State) {
        let bank = usize::from(state.romb0) | (usize::from(state.romb1) << 8);
        self.rom_offsets = (0, ROM_BANK_SIZE * bank);
    }

    fn recompute_huc1_offsets(&mut self, state: Huc1State) {
        self.rom_offsets = (0, ROM_BANK_SIZE * usize::from(state.rom_bank & 0x3F));
        self.ram_offset = RAM_BANK_SIZE * usize::from(state.ram_bank & 0x03);
    }

    fn rom_signature(&self) -> &[u8] {
        let start = 0x0100.min(self.rom.len());
        let end = 0x0150.min(self.rom.len());
        &self.rom[start..end]
    }
}

impl CartridgeType {
    fn from_header(value: u8, _rom: &[u8]) -> Result<Self, GbError> {
        Ok(match value {
            0x00 => Self::NoMbc {
                ram: false,
                battery: false,
            },
            0x08 => Self::NoMbc {
                ram: true,
                battery: false,
            },
            0x09 => Self::NoMbc {
                ram: true,
                battery: true,
            },
            0x01 => Self::Mbc1 {
                ram: false,
                battery: false,
                multicart: false,
            },
            0x02 => Self::Mbc1 {
                ram: true,
                battery: false,
                multicart: false,
            },
            0x03 => Self::Mbc1 {
                ram: true,
                battery: true,
                multicart: false,
            },
            0x05 => Self::Mbc2 { battery: false },
            0x06 => Self::Mbc2 { battery: true },
            0x0F => Self::Mbc3 {
                ram: false,
                battery: true,
                rtc: true,
            },
            0x10 => Self::Mbc3 {
                ram: true,
                battery: true,
                rtc: true,
            },
            0x11 => Self::Mbc3 {
                ram: false,
                battery: false,
                rtc: false,
            },
            0x12 => Self::Mbc3 {
                ram: true,
                battery: false,
                rtc: false,
            },
            0x13 => Self::Mbc3 {
                ram: true,
                battery: true,
                rtc: false,
            },
            0x19 => Self::Mbc5 {
                ram: false,
                battery: false,
                rumble: false,
            },
            0x1A => Self::Mbc5 {
                ram: true,
                battery: false,
                rumble: false,
            },
            0x1B => Self::Mbc5 {
                ram: true,
                battery: true,
                rumble: false,
            },
            0x1C => Self::Mbc5 {
                ram: false,
                battery: false,
                rumble: true,
            },
            0x1D => Self::Mbc5 {
                ram: true,
                battery: false,
                rumble: true,
            },
            0x1E => Self::Mbc5 {
                ram: true,
                battery: true,
                rumble: true,
            },
            0xFF => Self::Huc1,
            _ => return Err(GbError::UnsupportedCartridgeType(value)),
        })
    }

    fn has_external_ram(self) -> bool {
        match self {
            Self::NoMbc { ram, .. } => ram,
            Self::Mbc1 { ram, .. } => ram,
            Self::Mbc2 { .. } => false,
            Self::Mbc3 { ram, .. } => ram,
            Self::Mbc5 { ram, .. } => ram,
            Self::Huc1 => true,
        }
    }

    fn has_battery(self) -> bool {
        match self {
            Self::NoMbc { battery, .. } => battery,
            Self::Mbc1 { battery, .. } => battery,
            Self::Mbc2 { battery } => battery,
            Self::Mbc3 { battery, .. } => battery,
            Self::Mbc5 { battery, .. } => battery,
            Self::Huc1 => true,
        }
    }
}

fn parse_title(rom: &[u8], header_offset: usize) -> Result<String, GbError> {
    let new_licensee = rom.get(header_offset + 0x14B).copied().unwrap_or(0) == 0x33;
    let title_range = if new_licensee {
        (header_offset + 0x134)..(header_offset + 0x13F)
    } else {
        (header_offset + 0x134)..(header_offset + 0x143)
    };
    let bytes = rom
        .get(title_range)
        .ok_or(GbError::InvalidRomLength(rom.len()))?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}
