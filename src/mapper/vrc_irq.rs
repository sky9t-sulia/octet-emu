//! The interrupt counter Konami put in the VRC boards, shared by every one of
//! them that has one. It is the same silicon in the VRC4, the VRC6 and the
//! VRC7, so it is written once here rather than three times.

/// The VRC4 interrupt counter. Scanline mode divides the CPU clock by 341/3;
/// cycle mode counts CPU cycles directly.
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Default)]
pub(super) struct VrcIrq {
    pub(super) reload: u8,
    counter: u8,
    prescaler: i16,
    enabled: bool,
    enabled_after_ack: bool,
    cycle_mode: bool,
    pub(super) flag: bool,
}

impl VrcIrq {
    pub(super) fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        self.prescaler -= 3;
        if self.cycle_mode || self.prescaler <= 0 {
            if self.counter == 0xff {
                self.counter = self.reload;
                self.flag = true;
            } else {
                self.counter += 1;
            }
            self.prescaler += 341;
        }
    }

    pub(super) fn set_control(&mut self, val: u8) {
        self.enabled_after_ack = val & 0x01 != 0;
        self.enabled = val & 0x02 != 0;
        self.cycle_mode = val & 0x04 != 0;
        if self.enabled {
            self.counter = self.reload;
            self.prescaler = 341;
        }
        self.flag = false;
    }

    /// Acknowledging restores the enable bit that was parked by the control
    /// write, which is how the handler re-arms without a second write.
    pub(super) fn acknowledge(&mut self) {
        self.enabled = self.enabled_after_ack;
        self.flag = false;
    }
}

