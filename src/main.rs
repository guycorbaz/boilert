//! Main entry point for the boilert application.
//! Orchestrates sensor reading, MQTT publishing, and Slint UI updates.

mod config;
mod sensors;

use std::error::Error;
use slint::ComponentHandle;
use std::time::Duration;
use tokio::time;

slint::include_modules!();

// --- History Management ---
const HISTORY_POINTS: usize = 96; // 24 hours * 4 points/hour

struct SensorHistory {
    points: Vec<f32>,
    last_update: std::time::Instant,
}

impl SensorHistory {
    fn new(initial_val: f32) -> Self {
        Self {
            points: vec![initial_val; HISTORY_POINTS],
            last_update: std::time::Instant::now(),
        }
    }

    fn add_point(&mut self, val: f32) {
        self.points.remove(0);
        self.points.push(val);
        self.last_update = std::time::Instant::now();
    }

    fn to_svg_path(&self) -> String {
        let mut path = String::new();
        for (i, &temp) in self.points.iter().enumerate() {
            // X: 0 to 95
            // Y: 0 (top) to 100 (bottom). Map 100°C to 0 and 0°C to 100.
            let x = i as f32;
            let y = (100.0 - temp).clamp(0.0, 100.0);
            if i == 0 {
                path.push_str(&format!("M {} {} ", x, y));
            } else {
                path.push_str(&format!("L {} {} ", x, y));
            }
        }
        path
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize the Slint window
    let ui = AppWindow::new()?;
    let ui_weak = ui.as_weak();

    // Load configuration from config.toml
    let config = config::Config::load()?;
    
    // Set application version from Cargo.toml
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    
    // MQTT Setup
    let mut mqttoptions = rumqttc::MqttOptions::new("boilert", &config.mqtt.host, config.mqtt.port);
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (client, mut eventloop) = rumqttc::AsyncClient::new(mqttoptions, 10);
    
    tokio::spawn(async move {
        loop {
            if let Err(e) = eventloop.poll().await {
                eprintln!("MQTT connection error: {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });

    // Initial UI setup
    let mut initial_sensors = Vec::new();
    for sensor in &config.sensors {
        initial_sensors.push(SensorData {
            name: sensor.name.clone().into(),
            value: 0.0,
            history_path: "".into(),
        });
    }
    ui.set_sensors(slint::ModelRc::from(initial_sensors.as_slice()));

    // Initialize history with current sensor values (read once)
    let mut history: Vec<SensorHistory> = Vec::new();
    for sensor in &config.sensors {
        let val = sensors::read_temperature(&sensor.id).unwrap_or(20.0);
        history.push(SensorHistory::new(val));
    }

    // Spawn the main sensor reading and UI update loop
    let sensor_config = config.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(2));
        let mut last_history_update = std::time::Instant::now();
        let history_update_interval = Duration::from_secs(15 * 60); // 15 minutes

        loop {
            interval.tick().await;
            
            let mut temps = Vec::new();
            for sensor in &sensor_config.sensors {
                let temp = match sensors::read_temperature(&sensor.id) {
                    Ok(temp) => temp,
                    Err(e) => {
                        eprintln!("Error reading sensor {}: {}", sensor.name, e);
                        0.0
                    }
                };
                temps.push(temp);

                let topic = format!("{}/{}", sensor_config.mqtt.base_topic, sensor.name);
                let payload = temp.to_string();
                let _ = client.publish(topic, rumqttc::QoS::AtLeastOnce, false, payload).await;
            }

            // Update history every 15 minutes
            let now = std::time::Instant::now();
            let update_history = now.duration_since(last_history_update) >= history_update_interval;
            if update_history {
                for (i, &temp) in temps.iter().enumerate() {
                    if i < history.len() {
                        history[i].add_point(temp);
                    }
                }
                last_history_update = now;
            }

            // Energy calculation
            let avg_temp: f32 = if temps.is_empty() { 0.0 } else { temps.iter().sum::<f32>() / temps.len() as f32 };
            let delta_t = (avg_temp - sensor_config.boiler.reference_temp_c).max(0.0);
            let energy_kwh = (sensor_config.boiler.volume_l * delta_t * sensor_config.boiler.energy_coefficient) / 1000.0;

            // Publish the total energy to a dedicated MQTT topic
            let energy_topic = format!("{}/energy", sensor_config.mqtt.base_topic);
            let _ = client.publish(energy_topic, rumqttc::QoS::AtLeastOnce, false, energy_kwh.to_string()).await;

            let _ = slint::invoke_from_event_loop({
                let ui_weak = ui_weak.clone();
                let temps = temps.clone();
                let history_paths: Vec<String> = history.iter().map(|h| h.to_svg_path()).collect();
                let sensor_names: Vec<String> = sensor_config.sensors.iter().map(|s| s.name.clone()).collect();
                move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        let mut sensor_data = Vec::new();
                        for i in 0..temps.len() {
                            sensor_data.push(SensorData {
                                name: sensor_names[i].clone().into(),
                                value: temps[i],
                                history_path: history_paths[i].clone().into(),
                            });
                        }
                        ui.set_sensors(slint::ModelRc::from(sensor_data.as_slice()));
                        ui.set_energy_kwh(energy_kwh);
                    }
                }
            });
        }
    });

    // Start the Slint UI main loop
    ui.run()?;

    Ok(())
}
