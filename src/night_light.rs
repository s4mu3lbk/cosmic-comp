// SPDX-License-Identifier: GPL-3.0-only
//
// Night light (color temperature shifting) support.
//
// The color conversion uses the CIE approximation of the Planckian locus from
// "Analytic approximation of the CIE locus in the CIE 1960 (u,v)-chromaticity
// diagram" (M. Krystek, 1983) and the sRGB transformation matrices, as
// commonly implemented by redshift and wlsunset.
//
// The solar position calculation follows the NOAA Solar Calculation Details
// (public domain): https://gml.noaa.gov/grad/solcalc/solareqns.PDF

use cosmic_settings_config::night_light::Config;

/// RGB channel multipliers for a given color temperature in Kelvin.
///
/// Returns values in `0.0..=1.0`, suitable for scaling an identity gamma ramp.
pub fn temperature_to_multipliers(kelvin: f32) -> [f32; 3] {
    let t = kelvin.clamp(1000.0, 40000.0);

    // Approximate the CIE 1960 chromaticity coordinate of the Planckian locus.
    let x = if t < 4000.0 {
        // Krystek polynomial for 1667 K <= T <= 4000 K
        -0.2661239e9 / (t * t * t) - 0.2343589e6 / (t * t) + 0.8776956e3 / t + 0.179910
    } else {
        // Krystek polynomial for 4000 K <= T <= 25000 K
        -3.0258469e9 / (t * t * t) + 2.1070379e6 / (t * t) + 0.2226347e3 / t + 0.240390
    };
    // Correlated y coordinate of the Planckian locus.
    let y = -3.0 * x * x + 2.87 * x - 0.275;

    // xyY (Y = 1) to XYZ.
    let x_xyz = x / y;
    let y_xyz = 1.0;
    let z_xyz = (1.0 - x - y) / y;

    // XYZ to linear sRGB (IEC 61966-2-1).
    let r = 3.2406 * x_xyz - 1.5372 * y_xyz - 0.4986 * z_xyz;
    let g = -0.9689 * x_xyz + 1.8758 * y_xyz + 0.0415 * z_xyz;
    let b = 0.0557 * x_xyz - 0.2040 * y_xyz + 1.0570 * z_xyz;

    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

/// Convert a color temperature into gamma ramps of `size` entries per channel.
pub fn temperature_to_ramps(kelvin: f32, size: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    if size == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let [mr, mg, mb] = temperature_to_multipliers(kelvin);
    let max = u16::MAX as f32;
    let scale = |multiplier: f32| -> Vec<u16> {
        if size == 1 {
            return vec![(multiplier * max) as u16];
        }
        (0..size)
            .map(|i| {
                let v = i as f32 / (size - 1) as f32;
                (v * multiplier * max) as u16
            })
            .collect()
    };

    (scale(mr), scale(mg), scale(mb))
}

/// Result of a sunrise/sunset computation for one day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SunCycle {
    /// Sunset and sunrise (local time, minutes after midnight, may wrap past midnight).
    Times {
        /// Sunset, local minutes after midnight.
        sunset: u16,
        /// Sunrise, local minutes after midnight.
        sunrise: u16,
    },
    /// The sun never rises on this day at this location (polar night).
    AlwaysDown,
    /// The sun never sets on this day at this location (midnight sun).
    AlwaysUp,
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

/// Civil date (year, month, day) for days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Compute sunset and sunrise times for a date at a location.
///
/// `day` is days since 1970-01-01 (UTC). Times are returned in local minutes
/// after midnight, using the system's current UTC offset.
pub fn sun_cycle(day: i64, latitude: f64, longitude: f64) -> SunCycle {
    let (year, _month, _day_of_month) = civil_from_days(day);
    let year = year as f64;

    // Day of year (1-based).
    let day_of_year = (day - days_from_civil(year as i64, 1, 1) + 1) as f64;
    let gamma = 2.0 * std::f64::consts::PI / 365.0 * (day_of_year - 1.0 + (hour_of_day_utc() - 12.0) / 24.0);

    // Equation of time (minutes) and solar declination (radians).
    let eqtime = 229.18
        * (0.000075 + 0.001868 * gamma.cos() - 0.032077 * gamma.sin()
            - 0.014615 * (2.0 * gamma).cos()
            - 0.040849 * (2.0 * gamma).sin());
    let decl = 0.006918 - 0.399912 * gamma.cos() + 0.070257 * gamma.sin()
        - 0.006758 * (2.0 * gamma).cos()
        + 0.000907 * (2.0 * gamma).sin()
        - 0.002697 * (3.0 * gamma).cos()
        + 0.00148 * (3.0 * gamma).sin();

    let lat = latitude.to_radians();

    // Hour angle for the official sunset/sunrise zenith (90.833°), accounting
    // for atmospheric refraction and the solar semidiameter.
    let cos_ha = (90.833_f64.to_radians().cos() / (lat.cos() * decl.cos()))
        - lat.tan() * decl.tan();

    if cos_ha > 1.0 {
        // Sun never rises.
        return SunCycle::AlwaysDown;
    }
    if cos_ha < -1.0 {
        // Sun never sets.
        return SunCycle::AlwaysUp;
    }

    let ha = cos_ha.acos().to_degrees();

    // Solar noon (minutes UTC), then sunrise/sunset in UTC minutes.
    let solar_noon = 720.0 - 4.0 * longitude - eqtime;
    let sunrise_utc = solar_noon - 4.0 * ha;
    let sunset_utc = solar_noon + 4.0 * ha;

    // Local offset in minutes (east positive).
    let offset = local_utc_offset_minutes() as f64;

    let to_local = |utc_minutes: f64| -> u16 {
        (((utc_minutes + offset).rem_euclid(1440.0)) as u16) % (24 * 60)
    };

    SunCycle::Times {
        sunset: to_local(sunset_utc),
        sunrise: to_local(sunrise_utc),
    }
}

fn hour_of_day_utc() -> f64 {
    jiff::Timestamp::now().as_second() as f64 / 3600.0 % 24.0
}

fn local_utc_offset_minutes() -> i64 {
    jiff::Zoned::now()
        .offset()
        .seconds()
        .try_into()
        .map(|s: i64| s / 60)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for the current local date.
fn local_day() -> i64 {
    let date = jiff::Zoned::now().date();
    days_from_civil(date.year() as i64, date.month() as u32, date.day() as u32)
}

/// The time-of-day portion of a minute-of-day value split into (day, minutes).
fn day_and_minutes() -> (i64, u16) {
    let zoned = jiff::Zoned::now();
    let minutes = zoned.hour() as u16 * 60 + zoned.minute() as u16;
    (local_day(), minutes)
}

/// Is the given minute-of-day inside the night period (start..end, wrapping past midnight)?
fn in_night_period(now: u16, start: u16, end: u16) -> bool {
    if start <= end {
        start <= now && now < end
    } else {
        now >= start || now < end
    }
}

/// Determine the currently active night light temperature, if any.
///
/// `location` is (latitude, longitude), required for the automatic
/// sunset-to-sunrise schedule. When absent, the manual schedule is used.
pub fn active_temperature(config: &Config, location: Option<(f64, f64)>) -> Option<f32> {
    if !config.enabled {
        return None;
    }
    if config.always_on {
        return Some(config.temperature);
    }

    let (_, now) = day_and_minutes();
    let active = if config.auto_schedule {
        match location {
            Some((lat, long)) => {
                let cycle = sun_cycle(local_day(), lat, long);
                match cycle {
                    SunCycle::AlwaysDown => true,
                    SunCycle::AlwaysUp => false,
                    SunCycle::Times { sunset, sunrise } => in_night_period(now, sunset, sunrise),
                }
            }
            None => in_night_period(now, config.schedule_start_minutes, config.schedule_end_minutes),
        }
    } else {
        in_night_period(now, config.schedule_start_minutes, config.schedule_end_minutes)
    };

    if active {
        Some(config.temperature)
    } else {
        None
    }
}

/// Compute the duration until the next schedule transition, if a timer is needed.
///
/// This drives the event loop timer that flips night light on/off at the
/// configured boundary even when nothing else changes.
pub fn next_transition(config: &Config, location: Option<(f64, f64)>) -> Option<std::time::Duration> {
    if !config.enabled {
        return None;
    }
    if config.always_on {
        return None;
    }

    let (day, now) = day_and_minutes();

    let next_boundary = |day: i64| -> Option<u16> {
        if config.auto_schedule {
            match location {
                Some((lat, long)) => match sun_cycle(day, lat, long) {
                    SunCycle::AlwaysDown => None, // stays active; re-check in a day
                    SunCycle::AlwaysUp => None,
                    SunCycle::Times { sunset, sunrise } => {
                        // Boundaries sorted within the day (both may be at any time).
                        let mut bounds = [sunset, sunrise];
                        bounds.sort_unstable();
                        bounds.into_iter().find(|b| *b > now)
                    }
                },
                None => {
                    let mut bounds = [config.schedule_start_minutes, config.schedule_end_minutes];
                    bounds.sort_unstable();
                    bounds.into_iter().find(|b| *b > now)
                }
            }
        } else {
            let mut bounds = [config.schedule_start_minutes, config.schedule_end_minutes];
            bounds.sort_unstable();
            bounds.into_iter().find(|b| *b > now)
        }
    };

    if let Some(boundary) = next_boundary(day) {
        let until = boundary.saturating_sub(now) as u64;
        return Some(std::time::Duration::from_secs(until * 60) + std::time::Duration::from_secs(30));
    }

    // Next boundary is tomorrow (or, for polar day/night, re-check tomorrow).
    if let Some(boundary) = next_boundary(day + 1) {
        let until = (1440 - now + boundary) as u64;
        return Some(std::time::Duration::from_secs(until * 60) + std::time::Duration::from_secs(30));
    }

    // Polar day/night: re-evaluate every 6 hours.
    Some(std::time::Duration::from_secs(6 * 3600))
}

/// Query the system location via GeoClue (blocking; call from a helper thread).
pub fn query_location() -> Option<(f64, f64)> {
    use zbus::zvariant::Value;

    let conn = zbus::blocking::Connection::system().ok()?;
    let manager = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.GeoClue2",
        "/org/freedesktop/GeoClue2/Manager",
        "org.freedesktop.GeoClue2.Manager",
    )
    .ok()?;
    let (client_path,): (zbus::zvariant::OwnedObjectPath,) = manager.call("GetClient", &()).ok()?;
    if client_path.as_str() == "/" {
        return None;
    }

    let props = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.GeoClue2",
        client_path.as_str(),
        "org.freedesktop.DBus.Properties",
    )
    .ok()?;

    let set = |name: &str, value: Value<'_>| {
        let _: Result<(), _> = props.call(
            "Set",
            &("org.freedesktop.GeoClue2.Client", name, value),
        );
    };
    set("DesktopId", Value::from("cosmic-comp"));
    set("AccuracyLevel", Value::from(4u32));

    let client = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.GeoClue2",
        client_path.as_str(),
        "org.freedesktop.GeoClue2.Client",
    )
    .ok()?;
    let _: Result<(), _> = client.call("Start", &());

    // Poll for a location fix for up to ~5 seconds.
    for _ in 0..10 {
        let (path,): (zbus::zvariant::OwnedObjectPath,) = props
            .call("Get", &("org.freedesktop.GeoClue2.Client", "Location"))
            .ok()?;
        if path.as_str() != "/" {
            let location = zbus::blocking::Proxy::new(
                &conn,
                "org.freedesktop.GeoClue2",
                path.as_str(),
                "org.freedesktop.DBus.Properties",
            )
            .ok()?;
            let (lat,): (f64,) = location
                .call("Get", &("org.freedesktop.GeoClue2.Location", "Latitude"))
                .ok()?;
            let (lon,): (f64,) = location
                .call("Get", &("org.freedesktop.GeoClue2.Location", "Longitude"))
                .ok()?;
            let _: Result<(), _> = client.call("Stop", &());
            return Some((lat, lon));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    let _: Result<(), _> = client.call("Stop", &());
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_settings_config::night_light::Config;

    #[test]
    fn always_on_applies_temperature_at_any_time() {
        let mut config = Config {
            enabled: true,
            always_on: true,
            auto_schedule: true,
            ..Default::default()
        };

        // Regardless of the schedule mode or location, always-on must be
        // active at any time of day.
        for location in [None, Some((52.5, 13.4))] {
            assert_eq!(
                active_temperature(&config, location),
                Some(config.temperature)
            );
        }

        config.auto_schedule = false;
        for location in [None, Some((52.5, 13.4))] {
            assert_eq!(
                active_temperature(&config, location),
                Some(config.temperature)
            );
        }
    }

    #[test]
    fn always_on_still_requires_master_toggle() {
        let config = Config {
            enabled: false,
            always_on: true,
            ..Default::default()
        };

        assert_eq!(active_temperature(&config, None), None);
        assert_eq!(next_transition(&config, None), None);
    }

    #[test]
    fn always_on_needs_no_transition_timer() {
        let config = Config {
            enabled: true,
            always_on: true,
            ..Default::default()
        };

        assert_eq!(next_transition(&config, None), None);
    }
}
