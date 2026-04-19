//! Game Boy machine scaffolding.

use gbbrain_core::{
    AddressSpace, Breakpoint, CpuRegisters, FrameBuffer, Machine, MachineControl, MachineSnapshot,
    MemoryRegion, RenderTarget, RunResult, StopReason, UnimplementedMachine,
};

/// Placeholder DMG machine used to define the initial API shape.
pub struct GbMachine {
    rom: Vec<u8>,
    ram: Vec<u8>,
    registers: CpuRegisters,
}

impl GbMachine {
    pub fn new(rom: Vec<u8>) -> Self {
        Self {
            rom,
            ram: vec![0; 0x10000],
            registers: CpuRegisters::default(),
        }
    }
}

impl Machine for GbMachine {
    type Error = UnimplementedMachine;

    fn control(&mut self) -> &mut dyn MachineControl<Error = Self::Error> {
        self
    }

    fn snapshot(&self) -> MachineSnapshot {
        MachineSnapshot {
            registers: self.registers.clone(),
        }
    }

    fn inspect_memory(&self, region: MemoryRegion, address: u32, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(address).ok()?;
        match region {
            MemoryRegion::Rom => self.rom.get(start..start.checked_add(len)?).map(|s| s.to_vec()),
            MemoryRegion::Ram => self.ram.get(start..start.checked_add(len)?).map(|s| s.to_vec()),
            MemoryRegion::AddressSpace(AddressSpace::System) => {
                self.ram.get(start..start.checked_add(len)?).map(|s| s.to_vec())
            }
            _ => None,
        }
    }

    fn render_frame(&self, _target: RenderTarget) -> Result<FrameBuffer, Self::Error> {
        Ok(FrameBuffer::new_rgba(160, 144))
    }
}

impl MachineControl for GbMachine {
    type Error = UnimplementedMachine;

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.registers = CpuRegisters::default();
        self.ram.fill(0);
        Ok(())
    }

    fn run(&mut self) -> Result<RunResult, Self::Error> {
        Ok(RunResult {
            stop_reason: StopReason::BreakpointHit,
        })
    }

    fn step_instruction(&mut self) -> Result<RunResult, Self::Error> {
        self.registers.pc = self.registers.pc.wrapping_add(1);
        Ok(RunResult {
            stop_reason: StopReason::StepComplete,
        })
    }

    fn add_breakpoint(&mut self, _breakpoint: Breakpoint) -> Result<(), Self::Error> {
        Ok(())
    }

    fn clear_breakpoints(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
