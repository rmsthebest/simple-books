//! Warm evenings: with [`crate::Preferences::warm_evenings`] on, the light
//! grows warmer through the evening, is at its warmest overnight, and goes
//! back to the warmth chosen for the day in the morning, as Nickel's
//! Natural Light does. Only the warmth changes, never the brightness.
//!
//! A change to the warmth while the evening's is in use holds until the
//! morning, leaving the day's warmth as it was.

/// The warmest the light goes.
pub(crate) const WARMEST: u8 = 10;

/// Minutes after midnight, local time.
const EVENING_STARTS: u32 = 19 * 60;
const WARMEST_FROM: u32 = 22 * 60;
const MORNING_STARTS: u32 = 6 * 60;
const DAY_FROM: u32 = 7 * 60;

/// How far the light is along from the day's warmth to the warmest, from
/// 0 to 1, at `clock` (seconds since 1970) with local time `offset`
/// seconds ahead of UTC.
pub(crate) fn share(clock: u64, offset: i32) -> f32 {
    let day = 24 * 60 * 60;
    let local = (i64::try_from(clock).unwrap_or(0) + i64::from(offset)).rem_euclid(day);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "below a day's seconds"
    )]
    let minute = (local / 60) as u32;
    let along = |from: u32, to: u32| (minute - from) as f32 / (to - from) as f32;
    match minute {
        m if m < MORNING_STARTS => 1.0,
        m if m < DAY_FROM => 1.0 - along(MORNING_STARTS, DAY_FROM),
        m if m < EVENING_STARTS => 0.0,
        m if m < WARMEST_FROM => along(EVENING_STARTS, WARMEST_FROM),
        _ => 1.0,
    }
}

/// The warmth `share` of the way from `day` to the warmest.
pub(crate) fn warmth(day: u8, share: f32) -> u8 {
    let day = day.min(WARMEST);
    let more = f32::from(WARMEST - day) * share.clamp(0.0, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "between 0 and WARMEST - day"
    )]
    let more = more.round() as u8;
    day + more
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60;
    /// 2026-11-11, midnight UTC.
    const MIDNIGHT: u64 = 1_794_355_200;

    fn at(hours: f32) -> f32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let seconds = (hours * 3600.0) as u64;
        share(MIDNIGHT + seconds, 0)
    }

    #[test]
    fn the_day_is_as_chosen_and_the_night_is_warmest() {
        assert!((at(12.0)).abs() < f32::EPSILON);
        assert!((at(18.9)).abs() < f32::EPSILON);
        assert!((at(20.5) - 0.5).abs() < 0.01);
        assert!((at(22.0) - 1.0).abs() < f32::EPSILON);
        assert!((at(23.9) - 1.0).abs() < f32::EPSILON);
        assert!((at(3.0) - 1.0).abs() < f32::EPSILON);
        assert!((at(6.5) - 0.5).abs() < 0.01);
        assert!((at(7.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn local_time_is_what_counts() {
        // 20:00 UTC is 21:00 in Stockholm in winter: two thirds along.
        let share = share(MIDNIGHT + 20 * HOUR, 3600);
        assert!((share - 2.0 / 3.0).abs() < 0.01);
        // West of UTC, the evening is still to come.
        assert!(super::share(MIDNIGHT + 20 * HOUR, -5 * 3600).abs() < f32::EPSILON);
    }

    #[test]
    fn the_warmth_goes_from_the_days_to_the_warmest() {
        assert_eq!(warmth(4, 0.0), 4);
        assert_eq!(warmth(4, 0.5), 7);
        assert_eq!(warmth(4, 1.0), WARMEST);
        assert_eq!(warmth(WARMEST, 0.5), WARMEST);
        assert_eq!(warmth(0, 0.25), 3);
    }
}
