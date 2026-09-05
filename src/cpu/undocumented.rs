//! The 6502's incomplete instruction decode leaves gaps in the opcode
//! space; real software found some of what falls into them useful.

use super::*;

impl Cpu {
    /// The rest of the table: the opcodes the datasheet does not list, which
    /// are what falls out of the decoder rather than what was designed. Games
    /// use them, so they are here.
    #[rustfmt::skip]
    pub(super) fn execute_undocumented(&mut self, bus: &mut impl CpuBus, opcode: u8) {
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

        match opcode {
            // ---- NOPs with operands (they still perform their bus reads) ----
            0x1a | 0x3a | 0x5a | 0x7a | 0xda | 0xea | 0xfa => { self.read(bus, self.pc); }
            0x80 | 0x82 | 0x89 | 0xc2 | 0xe2 => { val!(imm); }
            0x04 | 0x44 | 0x64 => { val!(zp); }
            0x14 | 0x34 | 0x54 | 0x74 | 0xd4 | 0xf4 => { val!(zpx); }
            0x0c => { val!(abs); }
            0x1c | 0x3c | 0x5c | 0x7c | 0xdc | 0xfc => { val!(absx); }

            // ---- LAX: load A and X together ----
            0xa7 => { let v = val!(zp);   self.lax(v); }
            0xb7 => { let v = val!(zpy);  self.lax(v); }
            0xaf => { let v = val!(abs);  self.lax(v); }
            0xbf => { let v = val!(absy); self.lax(v); }
            0xa3 => { let v = val!(indx); self.lax(v); }
            0xb3 => { let v = val!(indy); self.lax(v); }

            // ---- SAX: store A AND X, without touching any flag ----
            0x87 => { let a = addr!(zp);   let v = self.a & self.x; self.write(bus, a, v); }
            0x97 => { let a = addr!(zpy);  let v = self.a & self.x; self.write(bus, a, v); }
            0x8f => { let a = addr!(abs);  let v = self.a & self.x; self.write(bus, a, v); }
            0x83 => { let a = addr!(indx); let v = self.a & self.x; self.write(bus, a, v); }

            // ---- SLO = ASL + ORA ----
            0x07 => { let a = addr!(zp);   self.slo(bus, a); }
            0x17 => { let a = addr!(zpx);  self.slo(bus, a); }
            0x0f => { let a = addr!(abs);  self.slo(bus, a); }
            0x1f => { let a = addr!(absx); self.slo(bus, a); }
            0x1b => { let a = addr!(absy); self.slo(bus, a); }
            0x03 => { let a = addr!(indx); self.slo(bus, a); }
            0x13 => { let a = addr!(indy); self.slo(bus, a); }

            // ---- RLA = ROL + AND ----
            0x27 => { let a = addr!(zp);   self.rla(bus, a); }
            0x37 => { let a = addr!(zpx);  self.rla(bus, a); }
            0x2f => { let a = addr!(abs);  self.rla(bus, a); }
            0x3f => { let a = addr!(absx); self.rla(bus, a); }
            0x3b => { let a = addr!(absy); self.rla(bus, a); }
            0x23 => { let a = addr!(indx); self.rla(bus, a); }
            0x33 => { let a = addr!(indy); self.rla(bus, a); }

            // ---- SRE = LSR + EOR ----
            0x47 => { let a = addr!(zp);   self.sre(bus, a); }
            0x57 => { let a = addr!(zpx);  self.sre(bus, a); }
            0x4f => { let a = addr!(abs);  self.sre(bus, a); }
            0x5f => { let a = addr!(absx); self.sre(bus, a); }
            0x5b => { let a = addr!(absy); self.sre(bus, a); }
            0x43 => { let a = addr!(indx); self.sre(bus, a); }
            0x53 => { let a = addr!(indy); self.sre(bus, a); }

            // ---- RRA = ROR + ADC ----
            0x67 => { let a = addr!(zp);   self.rra(bus, a); }
            0x77 => { let a = addr!(zpx);  self.rra(bus, a); }
            0x6f => { let a = addr!(abs);  self.rra(bus, a); }
            0x7f => { let a = addr!(absx); self.rra(bus, a); }
            0x7b => { let a = addr!(absy); self.rra(bus, a); }
            0x63 => { let a = addr!(indx); self.rra(bus, a); }
            0x73 => { let a = addr!(indy); self.rra(bus, a); }

            // ---- DCP = DEC + CMP ----
            0xc7 => { let a = addr!(zp);   self.dcp(bus, a); }
            0xd7 => { let a = addr!(zpx);  self.dcp(bus, a); }
            0xcf => { let a = addr!(abs);  self.dcp(bus, a); }
            0xdf => { let a = addr!(absx); self.dcp(bus, a); }
            0xdb => { let a = addr!(absy); self.dcp(bus, a); }
            0xc3 => { let a = addr!(indx); self.dcp(bus, a); }
            0xd3 => { let a = addr!(indy); self.dcp(bus, a); }

            // ---- ISC = INC + SBC ----
            0xe7 => { let a = addr!(zp);   self.isc(bus, a); }
            0xf7 => { let a = addr!(zpx);  self.isc(bus, a); }
            0xef => { let a = addr!(abs);  self.isc(bus, a); }
            0xff => { let a = addr!(absx); self.isc(bus, a); }
            0xfb => { let a = addr!(absy); self.isc(bus, a); }
            0xe3 => { let a = addr!(indx); self.isc(bus, a); }
            0xf3 => { let a = addr!(indy); self.isc(bus, a); }

            // ---- Immediate-only ALU oddities ----
            // ANC: AND, then copy bit 7 into carry as though an ASL followed.
            0x0b | 0x2b => {
                let v = val!(imm);
                self.a &= v;
                let r = self.a;
                self.set_zn(r);
                self.set_flag(C, r & 0x80 != 0);
            }
            // ALR: AND then LSR.
            0x4b => {
                let v = val!(imm);
                let t = self.a & v;
                self.a = self.lsr(t);
            }
            // ARR: AND then a ROR whose flags come from the adder, not the shifter.
            0x6b => {
                let v = val!(imm);
                let t = self.a & v;
                let out = (t >> 1) | ((self.flag(C) as u8) << 7);
                self.a = out;
                self.set_zn(out);
                self.set_flag(C, out & 0x40 != 0);
                self.set_flag(V, ((out >> 6) ^ (out >> 5)) & 0x01 != 0);
            }
            // SBX: X = (A AND X) - immediate, setting carry like a compare.
            0xcb => {
                let v = val!(imm);
                let lhs = self.a & self.x;
                let out = lhs.wrapping_sub(v);
                self.set_flag(C, lhs >= v);
                self.x = out;
                self.set_zn(out);
            }
            // ANE and LXA depend on analog behaviour of the chip; the constant
            // below is the value most NES-era hardware settles on.
            0x8b => {
                let v = val!(imm);
                self.a = (self.a | MAGIC) & self.x & v;
                let r = self.a;
                self.set_zn(r);
            }
            0xab => {
                let v = val!(imm);
                // LXA's constant differs from ANE's: with all bits set the OR
                // drops out and the operand passes straight through.
                self.a = (self.a | 0xff) & v;
                self.x = self.a;
                let r = self.a;
                self.set_zn(r);
            }

            // ---- Unstable stores: the value is ANDed with the address high
            // byte plus one, and a page crossing corrupts the address itself.
            0x9c => { let v = self.y; self.unstable_store(bus, v, self.x); }
            0x9e => { let v = self.x; self.unstable_store(bus, v, self.y); }
            0x9f => { let v = self.a & self.x; self.unstable_store(bus, v, self.y); }
            0x93 => {
                let ptr = self.fetch(bus);
                let lo = self.read(bus, ptr as u16) as u16;
                let hi = self.read(bus, ptr.wrapping_add(1) as u16) as u16;
                let base = (hi << 8) | lo;
                let v = self.a & self.x;
                self.unstable_store_at(bus, base, self.y, v);
            }
            // TAS also overwrites the stack pointer with A AND X.
            0x9b => {
                self.s = self.a & self.x;
                let v = self.s;
                self.unstable_store(bus, v, self.y);
            }
            // LAS: A, X and S all become the operand ANDed with S.
            0xbb => {
                let v = val!(absy);
                let r = v & self.s;
                self.a = r;
                self.x = r;
                self.s = r;
                self.set_zn(r);
            }

            // ---- JAM: the chip hangs with the bus floating. ----
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xb2 | 0xd2 | 0xf2 => {
                self.read(bus, self.pc);
                self.pc = self.pc.wrapping_sub(1); // spin in place forever
            }

            _ => unreachable!("opcode {opcode:#04x} is covered by the documented table"),
        }
    }

    fn lax(&mut self, val: u8) {
        self.a = val;
        self.x = val;
        self.set_zn(val);
    }

    fn slo(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = c.asl(v);
            c.a |= out;
            let r = c.a;
            c.set_zn(r);
            out
        });
    }

    fn rla(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = c.rol(v);
            c.a &= out;
            let r = c.a;
            c.set_zn(r);
            out
        });
    }

    fn sre(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = c.lsr(v);
            c.a ^= out;
            let r = c.a;
            c.set_zn(r);
            out
        });
    }

    fn rra(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = c.ror(v);
            c.adc(out);
            out
        });
    }

    fn dcp(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = v.wrapping_sub(1);
            let a = c.a;
            c.compare(a, out);
            out
        });
    }

    fn isc(&mut self, bus: &mut impl CpuBus, addr: u16) {
        self.rmw(bus, addr, |c, v| {
            let out = v.wrapping_add(1);
            c.sbc(out);
            out
        });
    }

    /// `SHY`/`SHX`/`SHA`/`TAS` with absolute,index addressing.
    fn unstable_store(&mut self, bus: &mut impl CpuBus, val: u8, idx: u8) {
        let base = self.fetch_u16(bus);
        self.unstable_store_at(bus, base, idx, val);
    }

    fn unstable_store_at(&mut self, bus: &mut impl CpuBus, base: u16, idx: u8, val: u8) {
        let addr = base.wrapping_add(idx as u16);
        self.read(bus, (base & 0xff00) | (addr & 0x00ff));
        let val = val & ((base >> 8) as u8).wrapping_add(1);
        // When the index carries, the high byte of the address is replaced by
        // the value being written.
        let addr = if (base & 0xff00) != (addr & 0xff00) {
            ((val as u16) << 8) | (addr & 0x00ff)
        } else {
            addr
        };
        self.write(bus, addr, val);
    }
}

/// The constant the accumulator is ORed with in `ANE`/`LXA`. It is a property
/// of the individual chip and its temperature; this is the common NES value.
const MAGIC: u8 = 0xee;
