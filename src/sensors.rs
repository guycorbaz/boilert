//! Sensor interface for reading 1-Wire temperature sensors.
//!
//! This module provides a unified interface for reading temperature data from DS18B20
//! sensors. It handles the low-level details of interacting with the Linux 1-Wire
//! bus when running on a Raspberry Pi, and provides a simulated data source for
//! development on other platforms.
//!
//! Note: reading a DS18B20 is slow (~750 ms per conversion at 12-bit resolution),
//! so callers should run [`read_temperature`] on a blocking thread
//! (e.g. `tokio::task::spawn_blocking`) and read several sensors in parallel.

use anyhow::{Context, Result};

#[cfg(feature = "pi")]
/// The base system path where 1-Wire device directories are located in Linux.
const W1_DIR: &str = "/sys/bus/w1/devices";

/// Parses the content of a DS18B20 `w1_slave` sysfs file.
///
/// Expected format (two lines):
/// ```text
/// 72 01 4b 46 7f ff 0e 10 57 : crc=57 YES
/// 72 01 4b 46 7f ff 0e 10 57 t=23125
/// ```
/// `YES` indicates a valid CRC and `t=` gives the temperature in millidegrees.
///
/// Kept separate from the sysfs access (and compiled on every platform) so it
/// can be unit-tested off-target.
#[cfg_attr(not(feature = "pi"), allow(dead_code))]
pub fn parse_w1_slave(content: &str) -> Result<f32> {
    if !content.contains("YES") {
        anyhow::bail!("CRC check failed");
    }
    let pos = content.find("t=").context("temperature marker 't=' not found")?;
    let raw = content[pos + 2..]
        .split_whitespace()
        .next()
        .context("temperature value missing after 't='")?;
    let temp_milli: f32 = raw
        .parse()
        .with_context(|| format!("invalid temperature value '{raw}'"))?;
    let temp = temp_milli / 1000.0;
    // Round to 2 decimal places
    Ok((temp * 100.0).round() / 100.0)
}

/// Reads the current temperature from a specific 1-Wire sensor.
///
/// This function is feature-gated:
/// - With `--features pi`: Reads directly from the `/sys/bus/w1/devices/<id>/w1_slave` file.
/// - Without `--features pi`: Returns a simulated slow random walk per sensor.
///
/// # Arguments
/// * `sensor_id` - The unique 1-Wire ID of the sensor (e.g., "28-000000000001").
///
/// # Returns
/// * `Result<f32>` - The temperature in Celsius, rounded to 2 decimal places.
#[cfg(feature = "pi")]
pub fn read_temperature(sensor_id: &str) -> Result<f32> {
    use std::fs;

    let path = format!("{W1_DIR}/{sensor_id}/w1_slave");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read sensor {sensor_id}"))?;
    parse_w1_slave(&content).with_context(|| format!("Failed to parse sensor {sensor_id}"))
}

/// Simulated sensor reading for development workstations.
///
/// Each sensor performs a slow random walk around its initial value, so the
/// history graphs look plausible instead of jumping around at every tick.
/// The first sensors queried (top of the tank) start hotter than the last ones.
#[cfg(not(feature = "pi"))]
pub fn read_temperature(sensor_id: &str) -> Result<f32> {
    use rand::RngExt;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static STATE: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut temps = state.lock().expect("simulation state poisoned");

    let mut rng = rand::rng();
    // Rank derived from the last digit of the sensor id, so the initial
    // stratification is stable regardless of the (parallel) read order.
    let rank = sensor_id
        .chars()
        .next_back()
        .and_then(|c| c.to_digit(10))
        .map_or(0.0, |d| d.saturating_sub(1) as f32);
    let temp = temps.entry(sensor_id.to_string()).or_insert_with(|| {
        // Stratified initial values: ~58 °C at the top, cooler further down.
        58.0 - 6.0 * rank + rng.random_range(-1.0..1.0)
    });
    // Slow drift, clamped to a realistic domestic hot water range.
    *temp = (*temp + rng.random_range(-0.15..0.15)).clamp(10.0, 75.0);

    // Round to 2 decimal places
    Ok((*temp * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str =
        "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=23125\n";

    #[test]
    fn parses_valid_output() {
        let temp = parse_w1_slave(VALID).unwrap();
        assert!((temp - 23.13).abs() < 1e-4);
    }

    #[test]
    fn parses_negative_temperature() {
        let content = "xx : crc=57 YES\nxx t=-1250\n";
        let temp = parse_w1_slave(content).unwrap();
        assert!((temp + 1.25).abs() < 1e-4);
    }

    #[test]
    fn rejects_bad_crc() {
        let content = VALID.replace("YES", "NO");
        assert!(parse_w1_slave(&content).is_err());
    }

    #[test]
    fn rejects_missing_temperature_marker() {
        assert!(parse_w1_slave("garbage YES\n").is_err());
    }

    #[test]
    fn rejects_malformed_temperature() {
        assert!(parse_w1_slave("xx YES\nxx t=abc\n").is_err());
    }
}
