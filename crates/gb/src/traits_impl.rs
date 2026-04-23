use std::cmp::Ordering;

use super::*;

impl Machine for GbMachine {
    type Error = GbError;

    fn control(&mut self) -> &mut dyn MachineControl<Error = Self::Error> {
        self
    }

    fn snapshot(&self) -> MachineSnapshot {
        MachineSnapshot {
            registers: self.snapshot_registers(),
            halted: matches!(self.exec_state, ExecState::Halt | ExecState::Stop),
            instruction_counter: self.instruction_counter,
        }
    }

    fn inspect_memory(&self, region: MemoryRegion, address: u32, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(address).ok()?;
        match region {
            MemoryRegion::Rom => self
                .cartridge
                .rom_data()
                .get(start..start.checked_add(len)?)
                .map(|s| s.to_vec()),
            MemoryRegion::Ram => self
                .wram
                .get(start..start.checked_add(len)?)
                .map(|s| s.to_vec())
                .or_else(|| {
                    self.cartridge
                        .ram_data()
                        .get(start..start.checked_add(len)?)
                        .map(|s| s.to_vec())
                }),
            MemoryRegion::Vram => self
                .vram
                .get(start..start.checked_add(len)?)
                .map(|s| s.to_vec()),
            MemoryRegion::Oam => self
                .oam
                .get(start..start.checked_add(len)?)
                .map(|s| s.to_vec()),
            MemoryRegion::AddressSpace(AddressSpace::System) => {
                self.system_memory_slice(u16::try_from(address).ok()?, len)
            }
        }
    }

    fn render_frame(&self, _target: RenderTarget) -> Result<FrameBuffer, Self::Error> {
        let mut frame = FrameBuffer::new_rgba(FRAME_WIDTH, FRAME_HEIGHT);

        let lcdc = self.io[(LCDC_REGISTER - IO_START) as usize];
        if lcdc & 0x80 == 0 {
            for pixel in frame.pixels_rgba8.chunks_exact_mut(4) {
                pixel[0] = 0xE0;
                pixel[1] = 0xE0;
                pixel[2] = 0xE0;
                pixel[3] = 0xFF;
            }
            return Ok(frame);
        }

        let bg_palette = self.io[0x47];
        let obj_palette0 = self.io[0x48];
        let obj_palette1 = self.io[0x49];
        let scroll_y = self.io[0x42];
        let scroll_x = self.io[0x43];
        let window_y = self.io[0x4A];
        let window_start_x = self.io[0x4B].saturating_sub(7);
        let mut bg_prio = vec![false; FRAME_WIDTH as usize * FRAME_HEIGHT as usize];

        for y in 0..FRAME_HEIGHT as usize {
            for x in 0..FRAME_WIDTH as usize {
                let mut intensity = 0xE0;

                if lcdc & 0x01 != 0 {
                    let bg_x = (x as u8).wrapping_add(scroll_x);
                    let bg_y = (y as u8).wrapping_add(scroll_y);
                    let raw =
                        self.fetch_bg_window_pixel(bg_x, bg_y, lcdc & 0x08 != 0, lcdc & 0x10 != 0);
                    bg_prio[y * FRAME_WIDTH as usize + x] = raw != 0;
                    intensity = palette_to_intensity(raw, bg_palette);
                }

                if lcdc & 0x21 == 0x21
                    && (y as u8) >= window_y
                    && (x as u8) >= window_start_x
                {
                    let win_x = (x as u8).wrapping_sub(window_start_x);
                    let win_y = (y as u8).wrapping_sub(window_y);
                    let raw = self.fetch_bg_window_pixel(
                        win_x,
                        win_y,
                        lcdc & 0x40 != 0,
                        lcdc & 0x10 != 0,
                    );
                    bg_prio[y * FRAME_WIDTH as usize + x] = raw != 0;
                    intensity = palette_to_intensity(raw, bg_palette);
                }

                write_intensity(&mut frame, x, y, intensity);
            }
        }

        if lcdc & 0x02 != 0 {
            let sprite_height = if lcdc & 0x04 != 0 { 16 } else { 8 };

            for screen_y in 0..FRAME_HEIGHT as u8 {
                let mut sprites = self
                    .oam
                    .chunks_exact(4)
                    .enumerate()
                    .filter_map(|(index, chunk)| {
                        let sprite_y = chunk[0].wrapping_sub(16);
                        if screen_y.wrapping_sub(sprite_y) >= sprite_height {
                            return None;
                        }
                        Some((
                            index,
                            chunk[1].wrapping_sub(8),
                            sprite_y,
                            chunk[2],
                            chunk[3],
                        ))
                    })
                    .take(10)
                    .collect::<Vec<_>>();

                sprites.sort_by(|a, b| match a.1.cmp(&b.1) {
                    Ordering::Equal => a.0.cmp(&b.0).reverse(),
                    other => other.reverse(),
                });

                for (_index, sprite_x, sprite_y, tile, flags) in sprites {
                    let rel_y = screen_y.wrapping_sub(sprite_y);

                    let mut tile_index = tile;
                    let mut tile_line = if flags & 0x40 != 0 {
                        sprite_height - 1 - rel_y
                    } else {
                        rel_y
                    };
                    if sprite_height == 16 {
                        tile_index &= 0xFE;
                        if tile_line >= 8 {
                            tile_index = tile_index.wrapping_add(1);
                            tile_line -= 8;
                        }
                    }

                    let row_addr = usize::from(tile_index) * 16 + usize::from(tile_line) * 2;
                    let data1 = self.vram[row_addr & 0x1FFF];
                    let data2 = self.vram[(row_addr + 1) & 0x1FFF];

                    for sprite_px in (0..8u8).rev() {
                        let target_x = sprite_x.wrapping_add(7 - sprite_px);
                        if target_x >= FRAME_WIDTH as u8 {
                            continue;
                        }

                        let bit = if flags & 0x20 != 0 {
                            7 - sprite_px
                        } else {
                            sprite_px
                        };
                        let raw = (((data2 >> bit) & 1) << 1) | ((data1 >> bit) & 1);
                        if raw == 0 {
                            continue;
                        }

                        let bg_index =
                            usize::from(screen_y) * FRAME_WIDTH as usize + usize::from(target_x);
                        if flags & 0x80 != 0 && bg_prio[bg_index] {
                            continue;
                        }

                        let palette = if flags & 0x10 != 0 {
                            obj_palette1
                        } else {
                            obj_palette0
                        };
                        write_intensity(
                            &mut frame,
                            usize::from(target_x),
                            usize::from(screen_y),
                            palette_to_intensity(raw, palette),
                        );
                    }
                }
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

fn write_intensity(frame: &mut FrameBuffer, x: usize, y: usize, intensity: u8) {
    let offset = (y * FRAME_WIDTH as usize + x) * 4;
    frame.pixels_rgba8[offset] = intensity;
    frame.pixels_rgba8[offset + 1] = intensity;
    frame.pixels_rgba8[offset + 2] = intensity;
    frame.pixels_rgba8[offset + 3] = 0xFF;
}

fn palette_to_intensity(color_index: u8, palette: u8) -> u8 {
    let shade = (palette >> (color_index * 2)) & 0x03;
    match shade {
        0 => 0xE0,
        1 => 0xA8,
        2 => 0x60,
        _ => 0x18,
    }
}

impl GbMachine {
    fn fetch_bg_window_pixel(&self, x: u8, y: u8, map_high: bool, unsigned_tiles: bool) -> u8 {
        let map_base = if map_high { 0x1C00 } else { 0x1800 };
        let row = usize::from(y / 8);
        let col = usize::from(x / 8);
        let tile_num = self.vram[((row * 32 + col) | map_base) & 0x1FFF];
        let tile_index = if unsigned_tiles {
            usize::from(tile_num)
        } else {
            usize::from(((tile_num as i8 as i16) + 128 + 128) as u16)
        };
        let line = usize::from(y % 8) * 2;
        let tile_base = tile_index * 16;
        let data1 = self.vram[(tile_base | line) & 0x1FFF];
        let data2 = self.vram[(tile_base | (line + 1)) & 0x1FFF];
        let bit = 7 - (x % 8);
        (((data2 >> bit) & 1) << 1) | ((data1 >> bit) & 1)
    }
}
