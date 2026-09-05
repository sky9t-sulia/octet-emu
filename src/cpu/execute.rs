//! Decoding one opcode byte into the addressing mode and primitive that
//! carry it out.

use super::*;

impl Cpu {
    /// One line per opcode, so the dispatch reads as the table it is.
    #[rustfmt::skip]
    pub(super) fn execute(&mut self, bus: &mut impl CpuBus, opcode: u8) {
        // Fetch the operand of a value-consuming instruction.
        macro_rules! val {
            (imm) => { self.fetch(bus) };
            (zp) => {{ let a = self.addr_zp(bus); self.read(bus, a) }};
            (zpx) => {{ let a = self.addr_zp_idx(bus, self.x); self.read(bus, a) }};
            (zpy) => {{ let a = self.addr_zp_idx(bus, self.y); self.read(bus, a) }};
            (abs) => {{ let a = self.addr_abs(bus); self.read(bus, a) }};
            (absx) => {{ let a = self.addr_abs_idx_read(bus, self.x); self.read(bus, a) }};
            (absy) => {{ let a = self.addr_abs_idx_read(bus, self.y); self.read(bus, a) }};
            (indx) => {{ let a = self.addr_ind_x(bus); self.read(bus, a) }};
            (indy) => {{ let a = self.addr_ind_y_read(bus); self.read(bus, a) }};
        }

        // Compute the effective address of a store or read-modify-write, which
        // always pays the index fixup cycle.
        macro_rules! addr {
            (zp) => { self.addr_zp(bus) };
            (zpx) => { self.addr_zp_idx(bus, self.x) };
            (zpy) => { self.addr_zp_idx(bus, self.y) };
            (abs) => { self.addr_abs(bus) };
            (absx) => { self.addr_abs_idx_write(bus, self.x) };
            (absy) => { self.addr_abs_idx_write(bus, self.y) };
            (indx) => { self.addr_ind_x(bus) };
            (indy) => { self.addr_ind_y_write(bus) };
        }

        /// Two-cycle instruction: the second cycle is a discarded fetch at PC.
        macro_rules! implied {
            ($body:block) => {{ self.read(bus, self.pc); $body }};
        }

        macro_rules! store {
            ($mode:ident, $reg:expr) => {{ let a = addr!($mode); let v = $reg; self.write(bus, a, v); }};
        }

        macro_rules! modify {
            ($mode:ident, $op:ident) => {{ let a = addr!($mode); self.rmw(bus, a, |c, v| c.$op(v)); }};
        }

        match opcode {
            // ---- Load / store ----
            0xa9 => { let v = val!(imm);  self.a = v; self.set_zn(v); }
            0xa5 => { let v = val!(zp);   self.a = v; self.set_zn(v); }
            0xb5 => { let v = val!(zpx);  self.a = v; self.set_zn(v); }
            0xad => { let v = val!(abs);  self.a = v; self.set_zn(v); }
            0xbd => { let v = val!(absx); self.a = v; self.set_zn(v); }
            0xb9 => { let v = val!(absy); self.a = v; self.set_zn(v); }
            0xa1 => { let v = val!(indx); self.a = v; self.set_zn(v); }
            0xb1 => { let v = val!(indy); self.a = v; self.set_zn(v); }

            0xa2 => { let v = val!(imm);  self.x = v; self.set_zn(v); }
            0xa6 => { let v = val!(zp);   self.x = v; self.set_zn(v); }
            0xb6 => { let v = val!(zpy);  self.x = v; self.set_zn(v); }
            0xae => { let v = val!(abs);  self.x = v; self.set_zn(v); }
            0xbe => { let v = val!(absy); self.x = v; self.set_zn(v); }

            0xa0 => { let v = val!(imm);  self.y = v; self.set_zn(v); }
            0xa4 => { let v = val!(zp);   self.y = v; self.set_zn(v); }
            0xb4 => { let v = val!(zpx);  self.y = v; self.set_zn(v); }
            0xac => { let v = val!(abs);  self.y = v; self.set_zn(v); }
            0xbc => { let v = val!(absx); self.y = v; self.set_zn(v); }

            0x85 => store!(zp,   self.a),
            0x95 => store!(zpx,  self.a),
            0x8d => store!(abs,  self.a),
            0x9d => store!(absx, self.a),
            0x99 => store!(absy, self.a),
            0x81 => store!(indx, self.a),
            0x91 => store!(indy, self.a),

            0x86 => store!(zp,  self.x),
            0x96 => store!(zpy, self.x),
            0x8e => store!(abs, self.x),

            0x84 => store!(zp,  self.y),
            0x94 => store!(zpx, self.y),
            0x8c => store!(abs, self.y),

            // ---- Transfers ----
            0xaa => implied!({ self.x = self.a; let v = self.x; self.set_zn(v); }),
            0xa8 => implied!({ self.y = self.a; let v = self.y; self.set_zn(v); }),
            0x8a => implied!({ self.a = self.x; let v = self.a; self.set_zn(v); }),
            0x98 => implied!({ self.a = self.y; let v = self.a; self.set_zn(v); }),
            0xba => implied!({ self.x = self.s; let v = self.x; self.set_zn(v); }),
            // TXS is the one transfer that does not touch the flags.
            0x9a => implied!({ self.s = self.x; }),

            // ---- Stack ----
            0x48 => { self.read(bus, self.pc); let v = self.a; self.push(bus, v); }
            0x08 => { self.read(bus, self.pc); let v = self.p | B | U; self.push(bus, v); }
            0x68 => {
                self.read(bus, self.pc);
                self.read(bus, 0x0100 | self.s as u16);
                self.a = self.pull(bus);
                let v = self.a;
                self.set_zn(v);
            }
            0x28 => {
                self.read(bus, self.pc);
                self.read(bus, 0x0100 | self.s as u16);
                // B is not a real bit of the register; U reads back as set.
                self.p = (self.pull(bus) & !B) | U;
            }

            // ---- Arithmetic and logic ----
            0x69 => { let v = val!(imm);  self.adc(v); }
            0x65 => { let v = val!(zp);   self.adc(v); }
            0x75 => { let v = val!(zpx);  self.adc(v); }
            0x6d => { let v = val!(abs);  self.adc(v); }
            0x7d => { let v = val!(absx); self.adc(v); }
            0x79 => { let v = val!(absy); self.adc(v); }
            0x61 => { let v = val!(indx); self.adc(v); }
            0x71 => { let v = val!(indy); self.adc(v); }

            0xe9 | 0xeb => { let v = val!(imm);  self.sbc(v); }
            0xe5 => { let v = val!(zp);   self.sbc(v); }
            0xf5 => { let v = val!(zpx);  self.sbc(v); }
            0xed => { let v = val!(abs);  self.sbc(v); }
            0xfd => { let v = val!(absx); self.sbc(v); }
            0xf9 => { let v = val!(absy); self.sbc(v); }
            0xe1 => { let v = val!(indx); self.sbc(v); }
            0xf1 => { let v = val!(indy); self.sbc(v); }

            0x29 => { let v = val!(imm);  self.a &= v; let r = self.a; self.set_zn(r); }
            0x25 => { let v = val!(zp);   self.a &= v; let r = self.a; self.set_zn(r); }
            0x35 => { let v = val!(zpx);  self.a &= v; let r = self.a; self.set_zn(r); }
            0x2d => { let v = val!(abs);  self.a &= v; let r = self.a; self.set_zn(r); }
            0x3d => { let v = val!(absx); self.a &= v; let r = self.a; self.set_zn(r); }
            0x39 => { let v = val!(absy); self.a &= v; let r = self.a; self.set_zn(r); }
            0x21 => { let v = val!(indx); self.a &= v; let r = self.a; self.set_zn(r); }
            0x31 => { let v = val!(indy); self.a &= v; let r = self.a; self.set_zn(r); }

            0x09 => { let v = val!(imm);  self.a |= v; let r = self.a; self.set_zn(r); }
            0x05 => { let v = val!(zp);   self.a |= v; let r = self.a; self.set_zn(r); }
            0x15 => { let v = val!(zpx);  self.a |= v; let r = self.a; self.set_zn(r); }
            0x0d => { let v = val!(abs);  self.a |= v; let r = self.a; self.set_zn(r); }
            0x1d => { let v = val!(absx); self.a |= v; let r = self.a; self.set_zn(r); }
            0x19 => { let v = val!(absy); self.a |= v; let r = self.a; self.set_zn(r); }
            0x01 => { let v = val!(indx); self.a |= v; let r = self.a; self.set_zn(r); }
            0x11 => { let v = val!(indy); self.a |= v; let r = self.a; self.set_zn(r); }

            0x49 => { let v = val!(imm);  self.a ^= v; let r = self.a; self.set_zn(r); }
            0x45 => { let v = val!(zp);   self.a ^= v; let r = self.a; self.set_zn(r); }
            0x55 => { let v = val!(zpx);  self.a ^= v; let r = self.a; self.set_zn(r); }
            0x4d => { let v = val!(abs);  self.a ^= v; let r = self.a; self.set_zn(r); }
            0x5d => { let v = val!(absx); self.a ^= v; let r = self.a; self.set_zn(r); }
            0x59 => { let v = val!(absy); self.a ^= v; let r = self.a; self.set_zn(r); }
            0x41 => { let v = val!(indx); self.a ^= v; let r = self.a; self.set_zn(r); }
            0x51 => { let v = val!(indy); self.a ^= v; let r = self.a; self.set_zn(r); }

            0xc9 => { let v = val!(imm);  let a = self.a; self.compare(a, v); }
            0xc5 => { let v = val!(zp);   let a = self.a; self.compare(a, v); }
            0xd5 => { let v = val!(zpx);  let a = self.a; self.compare(a, v); }
            0xcd => { let v = val!(abs);  let a = self.a; self.compare(a, v); }
            0xdd => { let v = val!(absx); let a = self.a; self.compare(a, v); }
            0xd9 => { let v = val!(absy); let a = self.a; self.compare(a, v); }
            0xc1 => { let v = val!(indx); let a = self.a; self.compare(a, v); }
            0xd1 => { let v = val!(indy); let a = self.a; self.compare(a, v); }

            0xe0 => { let v = val!(imm); let x = self.x; self.compare(x, v); }
            0xe4 => { let v = val!(zp);  let x = self.x; self.compare(x, v); }
            0xec => { let v = val!(abs); let x = self.x; self.compare(x, v); }

            0xc0 => { let v = val!(imm); let y = self.y; self.compare(y, v); }
            0xc4 => { let v = val!(zp);  let y = self.y; self.compare(y, v); }
            0xcc => { let v = val!(abs); let y = self.y; self.compare(y, v); }

            0x24 => { let v = val!(zp);  self.bit(v); }
            0x2c => { let v = val!(abs); self.bit(v); }

            // ---- Increment / decrement ----
            0xe6 => modify!(zp,   inc),
            0xf6 => modify!(zpx,  inc),
            0xee => modify!(abs,  inc),
            0xfe => modify!(absx, inc),
            0xc6 => modify!(zp,   dec),
            0xd6 => modify!(zpx,  dec),
            0xce => modify!(abs,  dec),
            0xde => modify!(absx, dec),

            0xe8 => implied!({ self.x = self.x.wrapping_add(1); let v = self.x; self.set_zn(v); }),
            0xc8 => implied!({ self.y = self.y.wrapping_add(1); let v = self.y; self.set_zn(v); }),
            0xca => implied!({ self.x = self.x.wrapping_sub(1); let v = self.x; self.set_zn(v); }),
            0x88 => implied!({ self.y = self.y.wrapping_sub(1); let v = self.y; self.set_zn(v); }),

            // ---- Shifts and rotates ----
            0x0a => implied!({ let v = self.a; self.a = self.asl(v); }),
            0x06 => modify!(zp,   asl),
            0x16 => modify!(zpx,  asl),
            0x0e => modify!(abs,  asl),
            0x1e => modify!(absx, asl),

            0x4a => implied!({ let v = self.a; self.a = self.lsr(v); }),
            0x46 => modify!(zp,   lsr),
            0x56 => modify!(zpx,  lsr),
            0x4e => modify!(abs,  lsr),
            0x5e => modify!(absx, lsr),

            0x2a => implied!({ let v = self.a; self.a = self.rol(v); }),
            0x26 => modify!(zp,   rol),
            0x36 => modify!(zpx,  rol),
            0x2e => modify!(abs,  rol),
            0x3e => modify!(absx, rol),

            0x6a => implied!({ let v = self.a; self.a = self.ror(v); }),
            0x66 => modify!(zp,   ror),
            0x76 => modify!(zpx,  ror),
            0x6e => modify!(abs,  ror),
            0x7e => modify!(absx, ror),

            // ---- Flags ----
            0x18 => implied!({ self.set_flag(C, false); }),
            0x38 => implied!({ self.set_flag(C, true); }),
            0x58 => implied!({ self.set_flag(I, false); }),
            0x78 => implied!({ self.set_flag(I, true); }),
            0xb8 => implied!({ self.set_flag(V, false); }),
            0xd8 => implied!({ self.set_flag(D, false); }),
            0xf8 => implied!({ self.set_flag(D, true); }),

            // ---- Branches ----
            0x10 => { let t = !self.flag(N); self.branch(bus, t); }
            0x30 => { let t =  self.flag(N); self.branch(bus, t); }
            0x50 => { let t = !self.flag(V); self.branch(bus, t); }
            0x70 => { let t =  self.flag(V); self.branch(bus, t); }
            0x90 => { let t = !self.flag(C); self.branch(bus, t); }
            0xb0 => { let t =  self.flag(C); self.branch(bus, t); }
            0xd0 => { let t = !self.flag(Z); self.branch(bus, t); }
            0xf0 => { let t =  self.flag(Z); self.branch(bus, t); }

            // ---- Jumps ----
            0x4c => { self.pc = self.addr_abs(bus); }
            0x6c => {
                let ptr = self.fetch_u16(bus);
                let lo = self.read(bus, ptr) as u16;
                // The high byte is fetched without carrying into the page, so
                // a pointer at $xxFF wraps back to $xx00.
                let hi = self.read(bus, (ptr & 0xff00) | (ptr.wrapping_add(1) & 0x00ff)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x20 => {
                let lo = self.fetch(bus) as u16;
                self.read(bus, 0x0100 | self.s as u16); // stack pre-read
                // The pushed address points at the last byte of JSR, not the
                // next instruction; RTS compensates by incrementing.
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                let hi = self.fetch(bus) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x60 => {
                self.read(bus, self.pc);
                self.read(bus, 0x0100 | self.s as u16);
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = (hi << 8) | lo;
                self.read(bus, self.pc);
                self.pc = self.pc.wrapping_add(1);
            }
            0x40 => {
                self.read(bus, self.pc);
                self.read(bus, 0x0100 | self.s as u16);
                self.p = (self.pull(bus) & !B) | U;
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x00 => {
                self.fetch(bus); // BRK reads and discards a padding byte
                self.push(bus, (self.pc >> 8) as u8);
                self.push(bus, self.pc as u8);

                // An NMI asserted during the push phase steals the vector.
                let vector = if self.need_nmi {
                    self.need_nmi = false;
                    NMI_VECTOR
                } else {
                    IRQ_VECTOR
                };
                self.push(bus, self.p | B | U);
                self.set_flag(I, true);
                let lo = self.read(bus, vector) as u16;
                let hi = self.read(bus, vector + 1) as u16;
                self.pc = (hi << 8) | lo;

                // The first instruction of the handler always runs before
                // another interrupt can be taken.
                self.prev_need_nmi = false;
            }

            _ => self.execute_undocumented(bus, opcode),
        }
    }
}

