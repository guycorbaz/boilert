//! Configuration management for the boilert application.
//! Handles loading and validating settings from a TOML file.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Maximum number of sensors supported by the UI (2-column, 3-row layout).
pub const MAX_SENSORS: usize = 6;

fn default_publish_interval() -> u64 {
    30
}

fn default_history_file() -> String {
    "boilert-history.json".to_string()
}

/// Configuration for a specific temperature sensor.
///
/// Sensors must be listed from the TOP of the tank to the BOTTOM: the first
/// entry is the highest probe. This ordering is used for the stratification
/// display on the dashboard.
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
    /// Seconds between two MQTT publications (default: 30).
    #[serde(default = "default_publish_interval")]
    pub publish_interval_s: u64,
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

/// UI options.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct UiConfig {
    /// Run the window fullscreen (kiosk mode on the touchscreen).
    #[serde(default)]
    pub fullscreen: bool,
}

/// The root configuration object for the application.
///
/// This struct is deserialized from a TOML file and contains all the settings
/// required to run the monitoring loop and connect to external services.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Settings for the MQTT broker connection.
    pub mqtt: MqttConfig,
    /// Physical characteristics and calculation constants for the water boiler.
    pub boiler: BoilerConfig,
    /// List of temperature sensors to monitor, ordered from top to bottom.
    pub sensors: Vec<SensorConfig>,
    /// Display options.
    #[serde(default)]
    pub ui: UiConfig,
    /// File used to persist the 24 h temperature history across restarts.
    #[serde(default = "default_history_file")]
    pub history_file: String,
}

impl Config {
    /// Loads, parses and validates the configuration from the given path.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read, if the TOML content is
    /// invalid, or if the values fail validation (see [`Config::validate`]).
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Self::from_toml(&content)
            .with_context(|| format!("Invalid configuration in {}", path.display()))
    }

    /// Parses and validates a configuration from a TOML string.
    pub fn from_toml(content: &str) -> Result<Self> {
        let config: Config = toml::from_str(content).context("Failed to parse TOML")?;
        config.validate()?;
        Ok(config)
    }

    /// Checks that the configured values are usable.
    fn validate(&self) -> Result<()> {
        use std::collections::HashSet;

        ensure!(
            !self.sensors.is_empty(),
            "at least one sensor must be configured"
        );
        ensure!(
            self.sensors.len() <= MAX_SENSORS,
            "at most {} sensors are supported, {} configured",
            MAX_SENSORS,
            self.sensors.len()
        );
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for sensor in &self.sensors {
            ensure!(!sensor.name.is_empty(), "sensor names must not be empty");
            ensure!(
                !sensor.name.contains(['/', '+', '#']),
                "sensor name '{}' contains an MQTT topic character (/ + #)",
                sensor.name
            );
            ensure!(!sensor.id.is_empty(), "sensor ids must not be empty");
            ensure!(
                names.insert(&sensor.name),
                "duplicate sensor name '{}': both would publish to the same MQTT topic",
                sensor.name
            );
            ensure!(
                ids.insert(&sensor.id),
                "duplicate sensor id '{}': the same probe cannot be at two positions",
                sensor.id
            );
        }
        ensure!(self.boiler.volume_l > 0.0, "boiler.volume_l must be positive");
        ensure!(
            self.boiler.energy_coefficient > 0.0,
            "boiler.energy_coefficient must be positive"
        );
        ensure!(
            (0.0..=40.0).contains(&self.boiler.reference_temp_c),
            "boiler.reference_temp_c must be between 0 and 40 °C (cold water baseline), got {}",
            self.boiler.reference_temp_c
        );
        ensure!(
            self.mqtt.publish_interval_s > 0,
            "mqtt.publish_interval_s must be positive"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_sensors(count: usize) -> String {
        let mut content = String::from(
            r#"
[mqtt]
host = "localhost"
port = 1883
base_topic = "boilert/sensors"

[boiler]
volume_l = 500.0
reference_temp_c = 15.0
energy_coefficient = 1.162
"#,
        );
        for i in 0..count {
            content.push_str(&format!(
                "\n[[sensors]]\nname = \"T{i}\"\nid = \"28-00000000000{i}\"\n"
            ));
        }
        content
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let config = Config::from_toml(&config_with_sensors(2)).unwrap();
        assert_eq!(config.sensors.len(), 2);
        assert_eq!(config.mqtt.publish_interval_s, 30);
        assert_eq!(config.history_file, "boilert-history.json");
        assert!(!config.ui.fullscreen);
    }

    #[test]
    fn rejects_empty_sensor_list() {
        assert!(Config::from_toml(&config_with_sensors(0)).is_err());
    }

    #[test]
    fn rejects_too_many_sensors() {
        assert!(Config::from_toml(&config_with_sensors(7)).is_err());
    }

    #[test]
    fn rejects_non_positive_volume() {
        let content = config_with_sensors(1).replace("volume_l = 500.0", "volume_l = 0.0");
        assert!(Config::from_toml(&content).is_err());
    }

    #[test]
    fn rejects_duplicate_sensor_names() {
        let content =
            config_with_sensors(1) + "\n[[sensors]]\nname = \"T0\"\nid = \"28-fffffffffff0\"\n";
        assert!(Config::from_toml(&content).is_err());
    }

    #[test]
    fn rejects_duplicate_sensor_ids() {
        let content =
            config_with_sensors(1) + "\n[[sensors]]\nname = \"Tbis\"\nid = \"28-000000000000\"\n";
        assert!(Config::from_toml(&content).is_err());
    }

    #[test]
    fn rejects_mqtt_topic_characters_in_sensor_names() {
        for bad in ["Top/1", "Top+", "Top#", ""] {
            let content =
                config_with_sensors(1).replace("name = \"T0\"", &format!("name = \"{bad}\""));
            assert!(
                Config::from_toml(&content).is_err(),
                "name {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_implausible_reference_temperature() {
        for bad in ["-5.0", "95.0"] {
            let content = config_with_sensors(1)
                .replace("reference_temp_c = 15.0", &format!("reference_temp_c = {bad}"));
            assert!(Config::from_toml(&content).is_err());
        }
    }
}
