use std::{env, fs, process::ExitCode};

use gbbrain_core::{Machine, MachineControl, MemoryRegion, RenderTarget};
use gbbrain_gb::GbMachine;

fn main() -> ExitCode {
    let rom_path = match env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: gbbrain <rom-path>");
            return ExitCode::from(2);
        }
    };

    let rom = match fs::read(&rom_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("failed to read ROM '{}': {error}", rom_path);
            return ExitCode::from(1);
        }
    };

    let mut machine = GbMachine::new(rom);
    let _ = machine.step_instruction();

    let snapshot = machine.snapshot();
    let rom_header = machine.inspect_memory(MemoryRegion::Rom, 0x100, 0x10);
    let frame = machine.render_frame(RenderTarget::Main).ok();

    println!("platform: GB");
    println!("pc: 0x{:04x}", snapshot.registers.pc);
    println!("rom_header_present: {}", rom_header.is_some());
    println!(
        "frame: {}",
        frame
            .map(|frame| format!("{}x{}", frame.width, frame.height))
            .unwrap_or_else(|| "unavailable".to_string())
    );

    ExitCode::SUCCESS
}
