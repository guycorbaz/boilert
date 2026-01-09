//! Configuration management for the boilert application.
//! Handles loading settings from `config.toml`.

use serde::Deserialize;
use std::fs;
use anyhow::{Context, Result};

/// Configuration for a specific temperature sensor.
#[derive(Debug, Deserialize, Clone)]
pub struct SensorConfig {
    /// Human-readable name of the sensor (e.g., "T1").
    pub name: String,
    /// 1-Wire device ID (e.g., "28-000000000001").
    pub id: String,
}

/// MQTT connection settings.
#[derive(Debug, Deserialize, Clone)]
pub struct MqttConfig {
    /// Hostname or IP of the MQTT broker.
    pub host: String,
    /// Port of the MQTT broker (usually 1883).
    pub port: u16,
    /// Base topic for publishing sensor data.
    pub base_topic: String,
}

/// Boiler physical and calculation parameters.
#[derive(Debug, Deserialize, Clone)]
pub struct BoilerConfig {
    /// Total volume of the boiler in liters.
    pub volume_l: f32,
    /// Reference temperature for energy calculation in Celsius.
    pub reference_temp_c: f32,
    /// Energy coefficient (Wh per liter per Kelvin). Default is usually 1.162.
    pub energy_coefficient: f32,
}

/// The root configuration object for the application.
/// 
/// This struct is deserialized from `config.toml` and contains all the settings 
/// required to run the monitoring loop and connect to external services.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Settings for the MQTT broker connection.
    pub mqtt: MqttConfig,
    /// Physical characteristics and calculation constants for the water boiler.
    pub boiler: BoilerConfig,
    /// List of temperature sensors to monitor.
    pub sensors: Vec<SensorConfig>,
}

impl Config {
    /// Loads and parses the configuration from `config.toml` in the current directory.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or if the TOML content is invalid.
    pub fn load() -> Result<Self> {
        let content = fs::read_to_string("config.toml")
            .context("Failed to read config.toml")?;
        let config: Config = toml::from_str(&content)
            .context("Failed to parse config.toml")?;
        Ok(config)
    }
}
