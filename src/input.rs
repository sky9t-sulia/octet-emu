//! Standard NES controllers.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    A,
    B,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    /// Bit position in the shift register, in the order the pad clocks them out.
    const fn bit(self) -> u8 {
        match self {
            Button::A => 0,
            Button::B => 1,
            Button::Select => 2,
            Button::Start => 3,
            Button::Up => 4,
            Button::Down => 5,
            Button::Left => 6,
            Button::Right => 7,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Controller {
    /// Live button state, updated by the frontend at any time.
    state: u8,
    /// Value latched into the pad's shift register at the last strobe.
    shift: u8,
    strobing: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub const fn new() -> Self {
        Controller { state: 0, shift: 0, strobing: false }
    }

    #[rustfmt::skip]
    pub fn set(&mut self, button: Button, pressed: bool) {
        let mask = 1 << button.bit();
        if pressed { self.state |= mask; } else { self.state &= !mask; }
    }

    pub fn strobe(&mut self, val: u8) {
        self.strobing = val & 1 != 0;
        if self.strobing {
            self.shift = self.state;
        }
    }

    pub fn read(&mut self) -> u8 {
        // While the strobe is high the pad's register is being reloaded every
        // moment, so a read cannot move it along: it reports the first button
        // and leaves the register holding whatever is held down now. Clocking
        // it here would leave the register one place along when the strobe
        // dropped, and the first real read would answer with B.
        if self.strobing {
            self.shift = self.state;
            return self.state & 1;
        }
        let bit = self.shift & 1;
        // Shifting in ones makes reads past the eighth return 1, as the real
        // open-collector pad does.
        self.shift = (self.shift >> 1) | 0x80;
        bit
    }
}
