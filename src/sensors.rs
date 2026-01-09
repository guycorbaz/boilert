//! Sensor interface for reading 1-Wire temperature sensors.
//!
//! This module provides a unified interface for reading temperature data from DS18B20 
//! sensors. It handles the low-level details of interacting with the Linux 1-Wire 
//! bus when running on a Raspberry Pi, and provides a simulated data source for 
//! development on other platforms.

#[cfg(feature = "pi")]
use anyhow::Context;
#[cfg(feature = "pi")]
use std::fs;

use anyhow::Result;

#[cfg(feature = "pi")]
/// The base system path where 1-Wire device directories are located in Linux.
const W1_DIR: &str = "/sys/bus/w1/devices";

/// Reads the current temperature from a specific 1-Wire sensor.
///
/// This function is feature-gated:
/// - With `--features pi`: Reads directly from the `/sys/bus/w1/devices/<id>/w1_slave` file.
/// - Without `--features pi`: Returns a random value between 20.0 and 30.0 for simulation.
///
/// # Arguments
/// * `_sensor_id` - The unique 1-Wire ID of the sensor (e.g., "28-000000000001").
///
/// # Returns
/// * `Result<f32>` - The temperature in Celsius, rounded to 2 decimal places.
pub fn read_temperature(_sensor_id: &str) -> Result<f32> {
    #[cfg(feature = "pi")]
    {
        // Real hardware reading (Raspberry Pi)
        let sensor_id = _sensor_id;
        let path = format!("{}/{}/w1_slave", W1_DIR, sensor_id);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read sensor {}", sensor_id))?;
        
        // The w1_slave file contains two lines.
        // Line 1: 72 01 4b 46 7f ff 0e 10 57 : crc=57 YES (YES indicates valid data)
        // Line 2: 72 01 4b 46 7f ff 0e 10 57 t=23125 (t is temperature in millidegrees)
        if !content.contains("YES") {
            return Err(anyhow::anyhow!("CRC check failed for sensor {}", sensor_id));
        }
        
        if let Some(pos) = content.find("t=") {
            let temp_str = &content[pos + 2..].trim();
            let temp_milli = temp_str.parse::<f32>()?;
            let temp = temp_milli / 1000.0;
            // Round to 2 decimal places
            Ok((temp * 100.0).round() / 100.0)
        } else {
            Err(anyhow::anyhow!("Temperature not found in sensor output"))
        }
    }

    #[cfg(not(feature = "pi"))]
    {
        // Dummy simulation for development workstation
        use rand::Rng;
        let mut rng = rand::thread_rng();
        // Generate a random temperature between 20°C and 30°C
        let temp: f32 = rng.gen_range(20.0..30.0);
        // Round to 2 decimal places
        Ok((temp * 100.0).round() / 100.0)
    }
}
