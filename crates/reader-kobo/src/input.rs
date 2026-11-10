//! Raw evdev events, and the touch panel's taps in screen pixels.
//!
//! The Clara BW's panel (`cyttsp5_mt`) uses multitouch protocol A, and
//! reports in landscape. From `docs/clara-bw-hardware.md`:
//!
//! - a touch-down sends `BTN_TOUCH` 1 and `ABS_MT_POSITION_X`/`_Y`, then
//!   `SYN_MT_REPORT` and `SYN_REPORT`;
//! - a lift sends `BTN_TOUCH` 0 and `SYN_REPORT`;
//! - no positions arrive in between, and there's no bounce;
//! - upright, screen x = 1071 − MT y and screen y = MT x.
//!
//! A tap is reported when the finger lifts, at the place it came down.

pub(crate) const EV_SYN: u16 = 0;
pub(crate) const EV_KEY: u16 = 1;
pub(crate) const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
pub(crate) const BTN_TOUCH: u16 = 0x14a;
pub(crate) const KEY_POWER: u16 = 116;

/// The kernel's `struct input_event`: a `timeval` of two longs, then the
/// type, code and value. 16 bytes on the Clara BW's 32-bit kernel.
pub(crate) const EVENT_SIZE: usize = 2 * size_of::<usize>() + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// The whole events in `bytes`, as read from an event device.
pub(crate) fn parse(bytes: &[u8]) -> impl Iterator<Item = RawEvent> + '_ {
    bytes.as_chunks::<EVENT_SIZE>().0.iter().map(|event| {
        let rest = &event[EVENT_SIZE - 8..];
        RawEvent {
            kind: u16::from_ne_bytes([rest[0], rest[1]]),
            code: u16::from_ne_bytes([rest[2], rest[3]]),
            value: i32::from_ne_bytes([rest[4], rest[5], rest[6], rest[7]]),
        }
    })
}

/// Follows the touch panel's reports from touch-down to lift.
#[derive(Debug, Default)]
pub(crate) struct Touch {
    /// The panel's raw axes, landscape.
    mt_x: Option<i32>,
    mt_y: Option<i32>,
    touching: bool,
    lifted: bool,
    /// Where the current touch came down, upright.
    down_at: Option<(f32, f32)>,
}

impl Touch {
    /// Takes one event. Returns a tap, at the place it came down, when the
    /// finger lifts. `width` is the upright screen's width in pixels.
    pub(crate) fn feed(&mut self, event: RawEvent, width: u32) -> Option<(f32, f32)> {
        match (event.kind, event.code) {
            (EV_ABS, ABS_MT_POSITION_X) => self.mt_x = Some(event.value),
            (EV_ABS, ABS_MT_POSITION_Y) => self.mt_y = Some(event.value),
            (EV_KEY, BTN_TOUCH) => {
                if event.value == 0 {
                    self.lifted = self.touching;
                } else {
                    self.touching = true;
                }
            }
            (EV_SYN, SYN_REPORT) => {
                if self.touching
                    && self.down_at.is_none()
                    && let (Some(mt_x), Some(mt_y)) = (self.mt_x, self.mt_y)
                {
                    let x = (width as i32 - 1 - mt_y).max(0);
                    self.down_at = Some((x as f32, mt_x.max(0) as f32));
                }
                if self.lifted {
                    let tap = self.down_at.take();
                    *self = Self::default();
                    return tap;
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYN_MT_REPORT: u16 = 2;
    const ABS_X: u16 = 0;
    const ABS_Y: u16 = 1;
    const ABS_PRESSURE: u16 = 0x18;
    const ABS_MT_TRACKING_ID: u16 = 0x39;
    const ABS_MT_PRESSURE: u16 = 0x3a;
    const BTN_TOOL_FINGER: u16 = 0x145;

    fn ev(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    /// What the panel sends for one tap, as recorded in Phase 0.
    fn tap(mt_x: i32, mt_y: i32) -> Vec<RawEvent> {
        vec![
            ev(EV_KEY, BTN_TOOL_FINGER, 1),
            ev(EV_KEY, BTN_TOUCH, 1),
            ev(EV_ABS, ABS_MT_TRACKING_ID, 0),
            ev(EV_ABS, ABS_MT_POSITION_X, mt_x),
            ev(EV_ABS, ABS_MT_POSITION_Y, mt_y),
            ev(EV_ABS, ABS_MT_PRESSURE, 1),
            ev(EV_ABS, ABS_X, 500),
            ev(EV_ABS, ABS_Y, 500),
            ev(EV_ABS, ABS_PRESSURE, 1024),
            ev(EV_SYN, SYN_MT_REPORT, 0),
            ev(EV_SYN, SYN_REPORT, 0),
            ev(EV_ABS, ABS_X, 0),
            ev(EV_ABS, ABS_Y, 0),
            ev(EV_ABS, ABS_PRESSURE, 0),
            ev(EV_KEY, BTN_TOOL_FINGER, 0),
            ev(EV_KEY, BTN_TOUCH, 0),
            ev(EV_SYN, SYN_REPORT, 0),
        ]
    }

    fn taps(events: &[RawEvent]) -> Vec<(f32, f32)> {
        let mut touch = Touch::default();
        events.iter().filter_map(|e| touch.feed(*e, 1072)).collect()
    }

    #[test]
    fn a_tap_lands_where_phase_0_measured_it() {
        // Targets drawn at (200, 300), (870, 1200) and (870, 300) read as
        // MT (296, 848), (1204, 178) and (298, 217).
        let mut events = tap(296, 848);
        events.extend(tap(1204, 178));
        events.extend(tap(298, 217));
        let found = taps(&events);
        let expected = [(200.0, 300.0), (870.0, 1200.0), (870.0, 300.0)];
        assert_eq!(found.len(), 3);
        for ((x, y), (ex, ey)) in found.iter().zip(expected) {
            assert!(
                (x - ex).abs() < 30.0 && (y - ey).abs() < 30.0,
                "({x}, {y}) for ({ex}, {ey})"
            );
        }
    }

    #[test]
    fn nothing_until_the_finger_lifts() {
        let events = tap(700, 500);
        assert_eq!(taps(&events[..11]), []);
        assert_eq!(taps(&events).len(), 1);
    }

    #[test]
    fn a_lift_without_a_position_is_no_tap() {
        let events = [
            ev(EV_KEY, BTN_TOUCH, 1),
            ev(EV_SYN, SYN_REPORT, 0),
            ev(EV_KEY, BTN_TOUCH, 0),
            ev(EV_SYN, SYN_REPORT, 0),
        ];
        assert_eq!(taps(&events), []);
        // And the next tap is unaffected.
        let mut more = events.to_vec();
        more.extend(tap(700, 500));
        assert_eq!(taps(&more), [(1071.0 - 500.0, 700.0)]);
    }

    #[test]
    fn events_parse_from_the_kernel_layout() {
        let mut bytes = vec![0u8; 2 * size_of::<usize>()];
        bytes.extend_from_slice(&EV_KEY.to_ne_bytes());
        bytes.extend_from_slice(&KEY_POWER.to_ne_bytes());
        bytes.extend_from_slice(&1i32.to_ne_bytes());
        // A partial event at the end is left for the next read.
        bytes.extend_from_slice(&[0; 5]);
        let events: Vec<_> = parse(&bytes).collect();
        assert_eq!(events, [ev(EV_KEY, KEY_POWER, 1)]);
    }
}
