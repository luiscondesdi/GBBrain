use gbbrain_core::{Breakpoint, Machine, MachineControl, MemoryRegion, RenderTarget, StopReason};

use super::{
    ExecState, GbError, GbMachine, IF_REGISTER, LCDC_REGISTER, LY_REGISTER, STAT_REGISTER,
    TIMER_DIV, TIMER_TAC, TIMER_TIMA, TIMER_TMA,
};

fn test_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x8000];
    rom[0x147] = 0x00;
    rom[0x148] = 0x00;
    rom[0x149] = 0x00;
    rom
}

fn mapped_rom(cartridge_type: u8, rom_size_header: u8, ram_size_header: u8) -> Vec<u8> {
    let banks = match rom_size_header {
        0x00 => 2,
        0x01 => 4,
        0x02 => 8,
        0x03 => 16,
        0x04 => 32,
        0x05 => 64,
        0x06 => 128,
        0x07 => 256,
        0x08 => 512,
        0x52 => 72,
        0x53 => 80,
        0x54 => 96,
        _ => panic!("unsupported ROM size header in test"),
    };
    let mut rom = vec![0; banks * 0x4000];
    rom[0x147] = cartridge_type;
    rom[0x148] = rom_size_header;
    rom[0x149] = ram_size_header;
    for bank in 0..banks {
        rom[bank * 0x4000] = bank as u8;
        rom[bank * 0x4000 + 1] = bank.wrapping_add(1) as u8;
    }
    rom
}

fn advance_startup_prefetch(machine: &mut GbMachine) {
    while machine.snapshot().registers.pc != 0x0100 {
        let result = machine.step_instruction().unwrap();
        assert_eq!(result.stop_reason, StopReason::StepComplete);
    }
    machine.instruction_counter = 0;
    machine.clear_trace();
    machine.pending_watchpoint = None;
}

#[test]
fn step_advances_pc_for_nop() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    advance_startup_prefetch(&mut machine);
    let result = machine.step_instruction().unwrap();
    let snapshot = machine.snapshot();

    assert_eq!(result.stop_reason, StopReason::StepComplete);
    assert_eq!(snapshot.registers.pc, 0x0101);
    assert_eq!(snapshot.instruction_counter, 1);
}

#[test]
fn program_counter_breakpoint_stops_run() {
    let mut rom = test_rom();
    rom[0x100] = 0x00;
    rom[0x101] = 0x00;

    let mut machine = GbMachine::new(rom).unwrap();
    machine
        .add_breakpoint(Breakpoint::ProgramCounter(0x0101))
        .unwrap();

    let result = machine.run().unwrap();

    assert_eq!(result.stop_reason, StopReason::BreakpointHit);
    assert_eq!(machine.snapshot().registers.pc, 0x0101);
}

#[test]
fn memory_write_watchpoint_triggers() {
    let mut rom = test_rom();
    rom[0x100] = 0x3E;
    rom[0x101] = 0x42;
    rom[0x102] = 0xEA;
    rom[0x103] = 0x00;
    rom[0x104] = 0xC0;

    let mut machine = GbMachine::new(rom).unwrap();
    machine
        .add_breakpoint(Breakpoint::MemoryWrite(0xC000))
        .unwrap();

    advance_startup_prefetch(&mut machine);
    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::StepComplete
    );
    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::WatchpointHit
    );
    assert_eq!(
        machine.inspect_memory(MemoryRegion::Ram, 0, 1).unwrap(),
        vec![0x42]
    );
}

#[test]
fn opcode_breakpoint_stops_before_execution() {
    let mut rom = test_rom();
    rom[0x100] = 0x00;
    rom[0x101] = 0x40;

    let mut machine = GbMachine::new(rom).unwrap();
    machine.add_breakpoint(Breakpoint::Opcode(0x40)).unwrap();

    advance_startup_prefetch(&mut machine);
    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::StepComplete
    );
    assert_eq!(machine.snapshot().registers.pc, 0x0101);

    let result = machine.run().unwrap();
    assert_eq!(result.stop_reason, StopReason::BreakpointHit);
    assert_eq!(machine.snapshot().registers.pc, 0x0101);
}

#[test]
fn direct_system_address_read_write_works() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0xC000, 0x42);

    assert_eq!(machine.read_system_address(0xC000), 0x42);
    assert_eq!(
        machine
            .inspect_memory(
                MemoryRegion::AddressSpace(gbbrain_core::AddressSpace::System),
                0xC000,
                1
            )
            .unwrap(),
        vec![0x42]
    );
}

#[test]
fn ei_enables_interrupts_after_following_instruction() {
    let mut rom = test_rom();
    rom[0x100] = 0xFB;
    rom[0x101] = 0x00;
    rom[0x102] = 0x00;

    let mut machine = GbMachine::new(rom).unwrap();
    advance_startup_prefetch(&mut machine);
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
    let mut rom = test_rom();
    rom[0x100] = 0x76;
    rom[0x101] = 0x00;
    rom[0x102] = 0x00;

    let mut machine = GbMachine::new(rom).unwrap();
    advance_startup_prefetch(&mut machine);
    machine.ie = 0x01;
    machine.request_interrupt(0x01);
    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::StepComplete
    );
    assert!(!matches!(machine.exec_state, ExecState::Halt));
    assert_eq!(machine.snapshot().registers.pc, 0x0101);

    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::StepComplete
    );
    assert_eq!(machine.snapshot().registers.pc, 0x0101);

    assert_eq!(
        machine.step_instruction().unwrap().stop_reason,
        StopReason::StepComplete
    );
    assert_eq!(machine.snapshot().registers.pc, 0x0102);
}

#[test]
fn trace_entries_record_executed_instructions() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    advance_startup_prefetch(&mut machine);
    machine.step_instruction().unwrap();
    machine.step_instruction().unwrap();

    let trace = machine.trace_entries();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].pc, 0x0100);
    assert_eq!(trace[1].instruction_counter, 2);
}

#[test]
fn serial_transfer_appends_output() {
    let mut rom = test_rom();
    rom[0x100..0x10B].copy_from_slice(&[
        0x3E, b'A', 0xEA, 0x01, 0xFF, 0x3E, 0x81, 0xEA, 0x02, 0xFF, 0x76,
    ]);

    let mut machine = GbMachine::new(rom).unwrap();
    assert!(machine.run().is_ok());
    assert_eq!(machine.serial_output(), b"A");
}

#[test]
fn timer_overflow_requests_interrupt() {
    let rom = test_rom();
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
fn lcd_enable_advances_to_first_scanline_after_initial_mode_window() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write8(LCDC_REGISTER, 0x00);
    machine.write8(LCDC_REGISTER, 0x81);
    machine.tick_timers(79);
    assert_eq!(machine.read8(LY_REGISTER), 0);
    assert_eq!(machine.read8(STAT_REGISTER) & 0x03, 0);
    machine.tick_timers(1);
    assert_eq!(machine.read8(LY_REGISTER), 1);
    assert_eq!(machine.read8(STAT_REGISTER) & 0x03, 2);
}

#[test]
fn bootrom_overlays_low_rom_until_ff50_disables_it() {
    let mut rom = test_rom();
    rom[0x0000] = 0xAA;
    rom[0x0100] = 0x76;
    let bootrom = vec![0x55; 0x100];

    let mut machine =
        GbMachine::new_with_model_and_bootrom(rom, super::GbModel::Dmg, Some(bootrom)).unwrap();

    assert_eq!(machine.read8(0x0000), 0x55);
    machine.write8(0xFF50, 0x01);
    assert_eq!(machine.read8(0x0000), 0xAA);
}

#[test]
fn bootrom_reset_uses_power_on_cpu_state() {
    let rom = test_rom();
    let bootrom = vec![0x00; 0x100];
    let mut machine =
        GbMachine::new_with_model_and_bootrom(rom, super::GbModel::Dmg, Some(bootrom)).unwrap();

    let snapshot = machine.snapshot();
    assert_eq!(snapshot.registers.pc, 0);
    assert_eq!(snapshot.registers.sp, 0);
    assert_eq!(machine.read8(0xFF41), 0x80);
}

#[test]
fn synthetic_bootrom_reaches_dmg_entry_state_by_execution() {
    let mut rom = test_rom();
    rom[0x0000] = 0xAA;
    let mut machine = GbMachine::new(rom).unwrap();

    advance_startup_prefetch(&mut machine);
    let snapshot = machine.snapshot();
    let debug = machine.debug_state();

    assert_eq!(snapshot.registers.a, 0x01);
    assert_eq!(snapshot.registers.f, 0xB0);
    assert_eq!(snapshot.registers.b, 0x00);
    assert_eq!(snapshot.registers.c, 0x13);
    assert_eq!(snapshot.registers.d, 0x00);
    assert_eq!(snapshot.registers.e, 0xD8);
    assert_eq!(snapshot.registers.h, 0x01);
    assert_eq!(snapshot.registers.l, 0x4D);
    assert_eq!(snapshot.registers.sp, 0xFFFE);
    assert_eq!(snapshot.registers.pc, 0x0100);
    assert_eq!(machine.read8(0x0000), 0xAA);
    assert_eq!(machine.read8(0xFF00), 0xCF);
    assert_eq!(machine.read8(0xFF0F), 0xE1);
    assert_eq!(machine.read8(0xFF26) & 0xF0, 0xF0);
    assert_eq!(machine.read8(0xFF40), 0x91);
    assert_eq!(machine.read8(0xFF47), 0xFC);
    assert_ne!(debug.div_counter, 0);
}

#[test]
fn stop_enters_stopped_state_and_illegal_opcodes_are_classified_explicitly() {
    let mut rom = test_rom();
    rom[0x100] = 0x10;
    rom[0x101] = 0x00;
    let mut machine = GbMachine::new(rom).unwrap();
    advance_startup_prefetch(&mut machine);
    let result = machine.step_instruction().unwrap();
    assert_eq!(result.stop_reason, StopReason::StepComplete);
    assert!(matches!(machine.exec_state, ExecState::Stop));

    let result = machine.step_instruction().unwrap();
    assert_eq!(result.stop_reason, StopReason::Halted);

    machine.write_system_address(0xFF00, 0x10);
    machine.set_pressed_buttons_mask(0x10);
    let result = machine.step_instruction().unwrap();
    assert_eq!(result.stop_reason, StopReason::StepComplete);
    assert_eq!(machine.snapshot().registers.pc, 0x0102);

    let mut rom = test_rom();
    rom[0x100] = 0xD3;
    let mut machine = GbMachine::new(rom).unwrap();
    advance_startup_prefetch(&mut machine);
    match machine.step_instruction() {
        Err(GbError::IllegalOpcode { opcode, pc }) => {
            assert_eq!(opcode, 0xD3);
            assert_eq!(pc, 0x0100);
        }
        other => panic!("expected illegal opcode error, got {other:?}"),
    }
}

#[test]
fn mbc1_switches_rom_and_ram_banks() {
    let rom = mapped_rom(0x03, 0x01, 0x03);
    let mut machine = GbMachine::new(rom).unwrap();

    assert_eq!(machine.read_system_address(0x4000), 1);

    machine.write_system_address(0x2000, 0x02);
    assert_eq!(machine.read_system_address(0x4000), 2);

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x6000, 0x01);
    machine.write_system_address(0x4000, 0x01);
    machine.write_system_address(0xA000, 0x42);
    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x24);
    machine.write_system_address(0x4000, 0x01);

    assert_eq!(machine.read_system_address(0xA000), 0x42);
    machine.write_system_address(0x4000, 0x00);
    assert_eq!(machine.read_system_address(0xA000), 0x24);
}

#[test]
fn mbc2_uses_low_nibble_ram_and_addressed_rom_banking() {
    let rom = mapped_rom(0x06, 0x01, 0x00);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0xA123, 0xBC);
    assert_eq!(machine.read_system_address(0xA123), 0xFC);

    machine.write_system_address(0x2100, 0x03);
    assert_eq!(machine.read_system_address(0x4000), 3);
}

#[test]
fn mbc3_rtc_latches_and_survives_save_state() {
    let rom = mapped_rom(0x10, 0x01, 0x03);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x08);
    machine.write_system_address(0xA000, 59);
    machine.write_system_address(0x4000, 0x09);
    machine.write_system_address(0xA000, 0);
    machine.write_system_address(0x4000, 0x0A);
    machine.write_system_address(0xA000, 0);

    for _ in 0..64 {
        machine.tick_timers(65_535);
    }
    machine.tick_timers(64);
    machine.write_system_address(0x6000, 0x00);
    machine.write_system_address(0x6000, 0x01);

    machine.write_system_address(0x4000, 0x08);
    assert_eq!(machine.read_system_address(0xA000), 0);
    machine.write_system_address(0x4000, 0x09);
    assert_eq!(machine.read_system_address(0xA000), 1);

    let snapshot = machine.save_state().unwrap();
    let mut restored = GbMachine::load_state(&snapshot).unwrap();
    restored.write_system_address(0x4000, 0x09);
    assert_eq!(restored.read_system_address(0xA000), 1);
}

#[test]
fn mbc5_supports_ninth_rom_bank_bit() {
    let mut rom = mapped_rom(0x1B, 0x08, 0x03);
    rom[256 * 0x4000] = 0xA5;
    rom[256 * 0x4000 + 1] = 0x5A;
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x2000, 0x00);
    machine.write_system_address(0x3000, 0x01);

    assert_eq!(machine.read_system_address(0x4000), 0xA5);
    assert_eq!(machine.read_system_address(0x4001), 0x5A);
}

#[test]
fn mbc5_supports_non_power_of_two_rom_sizes() {
    let rom = mapped_rom(0x19, 0x52, 0x00);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x2000, 70);
    assert_eq!(machine.read_system_address(0x4000), 70);

    machine.write_system_address(0x2000, 71);
    assert_eq!(machine.read_system_address(0x4000), 71);
}

#[test]
fn mbc3_supports_extended_ram_bank_mapping() {
    let rom = mapped_rom(0x10, 0x01, 0x05);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x04);
    machine.write_system_address(0xA000, 0x66);
    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x11);
    machine.write_system_address(0x4000, 0x04);

    assert_eq!(machine.read_system_address(0xA000), 0x66);
    machine.write_system_address(0x4000, 0x00);
    assert_eq!(machine.read_system_address(0xA000), 0x11);
}

#[test]
fn huc1_switches_rom_and_ram_banks() {
    let rom = mapped_rom(0xFF, 0x01, 0x03);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x2000, 0x03);
    assert_eq!(machine.read_system_address(0x4000), 3);

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x01);
    machine.write_system_address(0xA000, 0x99);
    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x33);
    machine.write_system_address(0x4000, 0x01);
    assert_eq!(machine.read_system_address(0xA000), 0x99);
    machine.write_system_address(0x4000, 0x00);
    assert_eq!(machine.read_system_address(0xA000), 0x33);
}

#[test]
fn joypad_reports_selected_buttons_and_requests_interrupt() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(IF_REGISTER, 0x00);
    machine.write_system_address(0xFF00, 0x10);
    machine.set_pressed_buttons_mask(0x10);

    assert_eq!(machine.read_system_address(0xFF00) & 0x0F, 0x0E);
    assert_ne!(machine.read_system_address(IF_REGISTER) & 0x10, 0);

    machine.write_system_address(IF_REGISTER, 0x00);
    machine.write_system_address(0xFF00, 0x20);
    machine.set_pressed_buttons_mask(0x08);
    assert_eq!(machine.read_system_address(0xFF00) & 0x0F, 0x07);
    assert_ne!(machine.read_system_address(IF_REGISTER) & 0x10, 0);

    machine.write_system_address(IF_REGISTER, 0x00);
    machine.set_pressed_buttons_mask(0x09);
    assert_ne!(machine.read_system_address(IF_REGISTER) & 0x10, 0);

    machine.write_system_address(IF_REGISTER, 0x00);
    machine.set_pressed_buttons_mask(0x01);
    machine.write_system_address(0xFF00, 0x10);
    assert_eq!(machine.read_system_address(IF_REGISTER) & 0x10, 0);
    machine.write_system_address(0xFF00, 0x20);
    assert_eq!(machine.read_system_address(0xFF00) & 0x0F, 0x0E);
    assert_ne!(machine.read_system_address(IF_REGISTER) & 0x10, 0);
}

#[test]
fn cartridge_state_round_trips_ram_and_rtc() {
    let mut rom = mapped_rom(0x10, 0x01, 0x03);
    rom[0x134..0x13B].copy_from_slice(b"TESTRTC");

    let mut machine = GbMachine::new(rom).unwrap();
    assert_eq!(machine.cartridge_title(), "TESTRTC");
    assert_eq!(machine.cartridge_type_code(), 0x10);
    assert!(machine.cartridge_has_battery());
    assert!(machine.cartridge_has_rtc());

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x44);
    machine.write_system_address(0x4000, 0x08);
    machine.write_system_address(0xA000, 12);

    let saved = machine.save_cartridge_state().unwrap();

    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x11);
    machine.write_system_address(0x4000, 0x08);
    machine.write_system_address(0xA000, 34);

    machine.load_cartridge_state(&saved).unwrap();
    machine.write_system_address(0x6000, 0x00);
    machine.write_system_address(0x6000, 0x01);
    machine.write_system_address(0x4000, 0x00);
    assert_eq!(machine.read_system_address(0xA000), 0x44);
    machine.write_system_address(0x4000, 0x08);
    assert_eq!(machine.read_system_address(0xA000), 12);
}

#[test]
fn cartridge_state_rejects_wrong_cartridge() {
    let mut rom_a = mapped_rom(0x03, 0x01, 0x03);
    rom_a[0x134..0x139].copy_from_slice(b"CARTA");
    let mut rom_b = mapped_rom(0x03, 0x01, 0x03);
    rom_b[0x134..0x139].copy_from_slice(b"CARTB");

    let machine_a = GbMachine::new(rom_a).unwrap();
    let state = machine_a.save_cartridge_state().unwrap();

    let mut machine_b = GbMachine::new(rom_b).unwrap();
    match machine_b.load_cartridge_state(&state) {
        Err(GbError::PersistentStateCartridgeMismatch) => {}
        other => panic!("expected cartridge mismatch, got {other:?}"),
    }
}

#[test]
fn reset_preserves_cartridge_ram_and_rtc_state() {
    let mut rom = mapped_rom(0x10, 0x01, 0x03);
    rom[0x134..0x13B].copy_from_slice(b"PERSIST");

    let mut machine = GbMachine::new(rom).unwrap();
    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x00);
    machine.write_system_address(0xA000, 0x5A);
    machine.write_system_address(0x4000, 0x08);
    machine.write_system_address(0xA000, 23);

    machine.reset().unwrap();

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0x4000, 0x00);
    assert_eq!(machine.read_system_address(0xA000), 0x5A);
    machine.write_system_address(0x4000, 0x08);
    machine.write_system_address(0x6000, 0x00);
    machine.write_system_address(0x6000, 0x01);
    assert_eq!(machine.read_system_address(0xA000), 23);
}

#[test]
fn raw_cartridge_ram_round_trips() {
    let rom = mapped_rom(0x03, 0x01, 0x03);
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(0x0000, 0x0A);
    machine.write_system_address(0xA000, 0x12);
    machine.write_system_address(0xA001, 0x34);

    let ram = machine.save_cartridge_ram();
    assert_eq!(ram[0], 0x12);
    assert_eq!(ram[1], 0x34);

    let mut replacement = vec![0; ram.len()];
    replacement[0] = 0xAB;
    replacement[1] = 0xCD;
    machine.load_cartridge_ram(&replacement).unwrap();

    machine.write_system_address(0x0000, 0x0A);
    assert_eq!(machine.read_system_address(0xA000), 0xAB);
    assert_eq!(machine.read_system_address(0xA001), 0xCD);
}

#[test]
fn lyc_write_does_not_trigger_stat_interrupt_on_match() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(IF_REGISTER, 0x00);
    machine.write_system_address(LCDC_REGISTER, 0x80);
    machine.write_system_address(STAT_REGISTER, 0x40);
    machine.write_system_address(0xFF45, 0x01);
    machine.write_system_address(IF_REGISTER, 0x00);
    machine.write_system_address(0xFF45, 0x00);
    machine.flush_pending_t34_interrupts();

    assert_eq!(machine.read_system_address(IF_REGISTER) & 0x02, 0);
}

#[test]
fn render_frame_reads_real_vram_background_data() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(LCDC_REGISTER, 0x91);
    machine.write_system_address(0xFF47, 0xE4);
    machine.write_system_address(0x8000, 0x80);
    machine.write_system_address(0x8001, 0x00);
    machine.write_system_address(0x9800, 0x00);

    let frame = machine.render_frame(RenderTarget::Main).unwrap();
    assert_eq!(frame.pixels_rgba8[0], 0xA8);
    assert_eq!(frame.pixels_rgba8[4], 0xE0);
}

#[test]
fn render_frame_limits_sprites_to_ten_per_scanline() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(LCDC_REGISTER, 0x82);
    machine.write_system_address(0xFF48, 0xE4);
    machine.write_system_address(0x8000, 0xFF);
    machine.write_system_address(0x8001, 0x00);

    for i in 0..11u16 {
        let oam = 0xFE00 + i * 4;
        machine.write_system_address(oam, 16);
        machine.write_system_address(oam + 1, 8 + (i as u8) * 8);
        machine.write_system_address(oam + 2, 0);
        machine.write_system_address(oam + 3, 0);
    }

    let frame = machine.render_frame(RenderTarget::Main).unwrap();
    let tenth_sprite_first_pixel = (0 * 160 + 72) * 4;
    let eleventh_sprite_first_pixel = (0 * 160 + 80) * 4;

    assert_eq!(frame.pixels_rgba8[tenth_sprite_first_pixel], 0xA8);
    assert_eq!(frame.pixels_rgba8[eleventh_sprite_first_pixel], 0xE0);
}

#[test]
fn render_frame_draws_window_when_wx_is_left_of_screen() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(LCDC_REGISTER, 0xF1);
    machine.write_system_address(0xFF47, 0xE4);
    machine.write_system_address(0xFF4A, 0x00);
    machine.write_system_address(0xFF4B, 0x00);

    machine.write_system_address(0x9800, 0x00);
    machine.write_system_address(0x9C00, 0x01);

    machine.write_system_address(0x8000, 0x00);
    machine.write_system_address(0x8001, 0x00);
    machine.write_system_address(0x8010, 0x80);
    machine.write_system_address(0x8011, 0x00);

    let frame = machine.render_frame(RenderTarget::Main).unwrap();
    assert_eq!(frame.pixels_rgba8[0], 0xA8);
    assert_eq!(frame.pixels_rgba8[4], 0xE0);
}

#[test]
fn render_frame_disables_window_when_bg_bit_is_clear() {
    let rom = test_rom();
    let mut machine = GbMachine::new(rom).unwrap();

    machine.write_system_address(LCDC_REGISTER, 0xF0);
    machine.write_system_address(0xFF47, 0xE4);
    machine.write_system_address(0xFF4A, 0x00);
    machine.write_system_address(0xFF4B, 0x07);
    machine.write_system_address(0x9C00, 0x01);
    machine.write_system_address(0x8010, 0x80);
    machine.write_system_address(0x8011, 0x00);

    let frame = machine.render_frame(RenderTarget::Main).unwrap();
    assert_eq!(frame.pixels_rgba8[0], 0xE0);
}

#[test]
#[ignore = "exploratory timing probe for Blargg init_timer; not a stable invariant yet"]
fn blargg_init_timer_window_matches_expected_if_edge() {
    let rom = test_rom();
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
