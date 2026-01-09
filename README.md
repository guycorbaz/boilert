# boilert 🌡️

`boilert` is a Water Boiler Monitoring application built with Rust and [Slint UI](https://slint.dev/). It monitors multiple 1-Wire temperature sensors, calculates stored energy, and publishes data to an MQTT broker.

## Features

- **Real-time Monitoring**: Visualizes from 1 to 6 temperature sensors simultaneously, depending on configuration.
- **Energy Calculation**: Automatically calculates the thermal energy stored in your boiler (kWh).
- **Temperature History**: Displays a 24-hour history graph for each sensor (15-minute resolution).
- **MQTT Integration**: Streams sensor data and energy metrics to your home automation system.
- **Dual Mode**: Runs in simulation mode on workstations or high-precision mode on Raspberry Pi.

---

## Prerequisites

- **Rust Toolchain**: [Install Rust](https://www.rust-lang.org/learn/get-started).
- **MQTT Broker**: Access to an MQTT broker (e.g., Mosquitto).
- **Hardware (Optional)**:
  - Raspberry Pi with 1-Wire interface enabled (`dtoverlay=w1-gpio`).
  - DS18B20 temperature sensors.

---

## Installation & Build

### 1. Clone the repository

```bash
git clone <your-repo-url>
cd boilert
```

### 2. Development / Simulation Mode

To run with simulated data (useful for UI testing):

```bash
cargo run
```

### 3. Raspberry Pi Mode

To build for real hardware, use the `pi` feature:

```bash
cargo build --release --features pi
# or run directly
cargo run --features pi
```

---

## Configuration

The application is configured via `config.toml` in the project root.

```toml
[mqtt]
host = "mqtt.home.arpa"
port = 1883
base_topic = "boilert/sensors"

[boiler]
volume_l = 500.0           # Total volume in Liters
reference_temp_c = 15.0    # Baseline cold water temperature
energy_coefficient = 1.162 # Wh/l·K (standard for water)

[[sensors]]
name = "Top"
id = "28-000000000001"     # 1-Wire device ID

[[sensors]]
name = "Bottom"
id = "28-000000000002"
```

---

## MQTT API

The application publishes data to the following topics:

| Topic | Description | Payload |
|-------|-------------|---------|
| `{base_topic}/{sensor_name}` | Temperature of a specific sensor | `f32` (Celsius) |
| `{base_topic}/energy` | Total energy stored in the boiler | `f32` (kWh) |

---

## Technical Details

### Energy Calculation

The application calculates energy using the formula:
`E (kWh) = (Volume (L) * ΔT (K) * 1.162) / 1000`
Where `ΔT` is the difference between the average temperature of all sensors and the `reference_temp_c`.

### History

- **Resolution**: 1 point every 15 minutes.
- **Capacity**: 96 points (24 hours).
- **Visualization**: Rendered as SVG paths within the Slint UI.

---

## License

MIT - See [LICENSE](LICENSE) for details.
