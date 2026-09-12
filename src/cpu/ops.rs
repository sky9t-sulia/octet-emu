//! Addressing modes, and the arithmetic and comparison primitives an
//! instruction's dispatch calls once it has one.

use super::*;

impl Cpu {
    pub(super) fn addr_zp(&mut self, bus: &mut impl CpuBus) -> u16 {
        self.fetch(bus) as u16
    }

    pub(super) fn addr_zp_idx(&mut self, bus: &mut impl CpuBus, idx: u8) -> u16 {
        let base = self.fetch(bus);
        self.read(bus, base as u16); // dummy read before the add
        base.wrapping_add(idx) as u16
    }

    pub(super) fn addr_abs(&mut self, bus: &mut impl CpuBus) -> u16 {
        self.fetch_u16(bus)
    }

    pub(super) fn addr_abs_idx_read(&mut self, bus: &mut impl CpuBus, idx: u8) -> u16 {
        let base = self.fetch_u16(bus);
        let addr = base.wrapping_add(idx as u16);
        if (base & 0xff00) != (addr & 0xff00) {
            self.read(bus, (base & 0xff00) | (addr & 0x00ff));
        }
        addr
    }

    pub(super) fn addr_abs_idx_write(&mut self, bus: &mut impl CpuBus, idx: u8) -> u16 {
        let base = self.fetch_u16(bus);
        let addr = base.wrapping_add(idx as u16);
        self.read(bus, (base & 0xff00) | (addr & 0x00ff));
        addr
    }

    pub(super) fn addr_ind_x(&mut self, bus: &mut impl CpuBus) -> u16 {
        let base = self.fetch(bus);
        self.read(bus, base as u16);
        let ptr = base.wrapping_add(self.x);
        // The pointer is fetched from zero page and wraps within it.
        let lo = self.read(bus, ptr as u16) as u16;
        let hi = self.read(bus, ptr.wrapping_add(1) as u16) as u16;
        (hi << 8) | lo
    }

    fn ind_y_base(&mut self, bus: &mut impl CpuBus) -> u16 {
        let ptr = self.fetch(bus);
        let lo = self.read(bus, ptr as u16) as u16;
        let hi = self.read(bus, ptr.wrapping_add(1) as u16) as u16;
        (hi << 8) | lo
    }

    pub(super) fn addr_ind_y_read(&mut self, bus: &mut impl CpuBus) -> u16 {
        let base = self.ind_y_base(bus);
        let addr = base.wrapping_add(self.y as u16);
        if (base & 0xff00) != (addr & 0xff00) {
            self.read(bus, (base & 0xff00) | (addr & 0x00ff));
        }
        addr
    }

    pub(super) fn addr_ind_y_write(&mut self, bus: &mut impl CpuBus) -> u16 {
        let base = self.ind_y_base(bus);
        let addr = base.wrapping_add(self.y as u16);
        self.read(bus, (base & 0xff00) | (addr & 0x00ff));
        addr
    }

    // ---- Operations -----------------------------------------------------

    /// A conditional branch.
    ///
    /// Not taken, it is two cycles and decides about interrupts where every
    /// other instruction does. Taken, it decides *before* it moves the
    /// program counter and then never looks again. An interrupt arriving
    /// during the cycles that follow waits for the next instruction, which is
    /// what AccuracyCoin's page 12 means by a branch not polling before its
    /// third cycle.
    ///
    /// That answer is held in a local rather than left sitting in the poll's
    /// shift register. Anything that adds cycles here polls as well: a DMA
    /// stealing the third cycle keeps sampling the lines while it holds the
    /// processor off the bus. Without the local it would push the branch's own
    /// answer out and take an interrupt this instruction had already declined.
    /// The lines still get sampled; it is only this instruction's verdict that
    /// is fixed.
    pub(super) fn branch(&mut self, bus: &mut impl CpuBus, take: bool) {
        let offset = self.fetch(bus) as i8 as u16;
        if !take {
            return;
        }
        let mut decided = self.prev_run_irq;

        self.read(bus, self.pc);
        let target = self.pc.wrapping_add(offset);
        if (target & 0xff00) != (self.pc & 0xff00) {
            // Crossing a page costs another cycle, and that one does poll:
            // either answer is enough to take the interrupt.
            decided |= self.run_irq;
            self.read(bus, (self.pc & 0xff00) | (target & 0x00ff));
        }
        self.pc = target;
        self.prev_run_irq = decided;
    }

    pub(super) fn compare(&mut self, lhs: u8, rhs: u8) {
        let diff = lhs.wrapping_sub(rhs);
        self.set_flag(C, lhs >= rhs);
        self.set_zn(diff);
    }

    pub(super) fn adc(&mut self, val: u8) {
        // The 2A03 has decimal mode disabled, so this is plain binary add.
        let sum = self.a as u16 + val as u16 + self.flag(C) as u16;
        let result = sum as u8;
        self.set_flag(C, sum > 0xff);
        // Overflow when both operands agree in sign but the result does not.
        self.set_flag(V, (self.a ^ result) & (val ^ result) & 0x80 != 0);
        self.a = result;
        self.set_zn(result);
    }

    pub(super) fn sbc(&mut self, val: u8) {
        self.adc(!val);
    }

    pub(super) fn asl(&mut self, val: u8) -> u8 {
        self.set_flag(C, val & 0x80 != 0);
        let out = val << 1;
        self.set_zn(out);
        out
    }

    pub(super) fn lsr(&mut self, val: u8) -> u8 {
        self.set_flag(C, val & 0x01 != 0);
        let out = val >> 1;
        self.set_zn(out);
        out
    }

    pub(super) fn rol(&mut self, val: u8) -> u8 {
        let out = (val << 1) | self.flag(C) as u8;
        self.set_flag(C, val & 0x80 != 0);
        self.set_zn(out);
        out
    }

    pub(super) fn ror(&mut self, val: u8) -> u8 {
        let out = (val >> 1) | ((self.flag(C) as u8) << 7);
        self.set_flag(C, val & 0x01 != 0);
        self.set_zn(out);
        out
    }

    /// Read-modify-write: the old value is written back first. Some mappers
    /// and registers see that write, so it cannot be skipped.
    pub(super) fn rmw(&mut self, bus: &mut impl CpuBus, addr: u16, op: impl FnOnce(&mut Self, u8) -> u8) {
        let val = self.read(bus, addr);
        self.write(bus, addr, val);
        let out = op(self, val);
        self.write(bus, addr, out);
    }
}


impl Cpu {
    pub(super) fn bit(&mut self, val: u8) {
        self.set_flag(Z, self.a & val == 0);
        // N and V come straight from the operand, not from the AND result.
        self.set_flag(N, val & 0x80 != 0);
        self.set_flag(V, val & 0x40 != 0);
    }

    pub(super) fn inc(&mut self, val: u8) -> u8 {
        let out = val.wrapping_add(1);
        self.set_zn(out);
        out
    }

    pub(super) fn dec(&mut self, val: u8) -> u8 {
        let out = val.wrapping_sub(1);
        self.set_zn(out);
        out
    }
}
