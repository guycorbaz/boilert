//! Main entry point for the boilert application.
//! Orchestrates sensor reading, MQTT publishing, and Slint UI updates.

mod config;
mod energy;
mod history;
mod sensors;

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use slint::ComponentHandle;
use tokio::time::{self, MissedTickBehavior};
use tracing::{error, info, warn};

slint::include_modules!();

/// Interval between sensor readings and UI refreshes.
const READ_INTERVAL: Duration = Duration::from_secs(2);
/// Interval between two history points (15-minute resolution).
const HISTORY_INTERVAL: Duration = Duration::from_secs(history::POINT_INTERVAL_S);

/// Maps a water temperature to a display color: blue (#3987e5) at 15 °C,
/// red (#e34948) at 65 °C — the diverging cold/hot pair of the palette.
fn temp_to_color(temp: f32) -> slint::Color {
    let f = ((temp - 15.0) / 50.0).clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| (a + (b - a) * f).round() as u8;
    slint::Color::from_rgb_u8(lerp(57.0, 227.0), lerp(135.0, 73.0), lerp(229.0, 72.0))
}

/// Color for a possibly-unknown temperature (gray when no reading yet).
fn color_for(temp: Option<f32>) -> slint::Color {
    match temp {
        Some(t) => temp_to_color(t),
        None => slint::Color::from_rgb_u8(110, 110, 110),
    }
}

/// Interpolates the temperature at relative tank height `p` (0.0 = top,
/// 1.0 = bottom) from the valid sensor readings, or `None` if there are none.
fn interpolate_at(known: &[(f32, f32)], p: f32) -> Option<f32> {
    let (first, last) = (known.first()?, known.last()?);
    if p <= first.0 {
        return Some(first.1);
    }
    if p >= last.0 {
        return Some(last.1);
    }
    for pair in known.windows(2) {
        let (p0, t0) = pair[0];
        let (p1, t1) = pair[1];
        if p <= p1 {
            let f = if p1 > p0 { (p - p0) / (p1 - p0) } else { 0.0 };
            return Some(t0 + (t1 - t0) * f);
        }
    }
    Some(last.1)
}

/// Computes the 6 gradient stop colors of the tank display (top to bottom)
/// by interpolating the valid sensor temperatures along the tank height.
/// The first configured sensor is at the top of the tank.
fn stratification_colors(last_valid: &[Option<f32>]) -> [slint::Color; 6] {
    let n = last_valid.len();
    let known: Vec<(f32, f32)> = last_valid
        .iter()
        .enumerate()
        .filter_map(|(i, temp)| {
            let pos = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.5 };
            temp.map(|t| (pos, t))
        })
        .collect();
    std::array::from_fn(|k| color_for(interpolate_at(&known, k as f32 / 5.0)))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // The configuration file path can be given as the first CLI argument
    // (useful for systemd deployments); defaults to ./config.toml.
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let config = config::Config::load(std::path::Path::new(&config_path))?;
    info!(
        "Configuration loaded from {config_path} ({} sensors)",
        config.sensors.len()
    );

    // Initialize the Slint window
    let ui = AppWindow::new()?;
    let ui_weak = ui.as_weak();

    // Set application version from Cargo.toml
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    if config.ui.fullscreen {
        ui.window().set_fullscreen(true);
    }

    // Initial sensor model so the UI shows the configured names before the
    // first reading completes.
    let initial: Vec<SensorData> = config
        .sensors
        .iter()
        .map(|s| SensorData {
            name: s.name.clone().into(),
            value_text: "--".into(),
            ok: true,
            history_path: "".into(),
            history_fill: "".into(),
            hist_min_text: "".into(),
            hist_max_text: "".into(),
        })
        .collect();
    ui.set_sensors(slint::ModelRc::from(initial.as_slice()));

    // --- MQTT setup ---
    let status_topic = format!("{}/status", config.mqtt.base_topic);
    // Unique client id so two instances don't evict each other from the broker.
    let client_id = format!("boilert-{}", std::process::id());
    let mut mqttoptions = rumqttc::MqttOptions::new(client_id, &config.mqtt.host, config.mqtt.port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    // Last will: the broker marks the device offline if the connection drops.
    mqttoptions.set_last_will(rumqttc::LastWill::new(
        &status_topic,
        "offline",
        rumqttc::QoS::AtLeastOnce,
        true,
    ));

    let (client, mut eventloop) = rumqttc::AsyncClient::new(mqttoptions, 64);
    let mqtt_connected = Arc::new(AtomicBool::new(false));

    // MQTT event loop task: drives the connection, tracks its state, and
    // announces availability on the status topic after each (re)connection.
    {
        let client = client.clone();
        let connected = mqtt_connected.clone();
        let status_topic = status_topic.clone();
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_))) => {
                        info!("Connected to MQTT broker");
                        connected.store(true, Ordering::Relaxed);
                        let _ = client
                            .publish(
                                status_topic.as_str(),
                                rumqttc::QoS::AtLeastOnce,
                                true,
                                "online",
                            )
                            .await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // Only log the transition to avoid flooding the journal.
                        if mqtt_was_connected(&connected) {
                            warn!("MQTT connection lost: {e}");
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    // --- Main sensor reading / publishing / UI update loop ---
    let sensor_config = config.clone();
    let connected = mqtt_connected.clone();
    tokio::spawn(async move {
        let sensor_count = sensor_config.sensors.len();
        let mut interval = time::interval(READ_INTERVAL);
        // Sensor reads can be slow; don't try to catch up on missed ticks.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // 24 h history, restored from disk when available.
        let mut hist = history::load_or_new(&sensor_config.history_file, sensor_count);
        let mut last_history_update = Instant::now();

        let publish_interval = Duration::from_secs(sensor_config.mqtt.publish_interval_s);
        let mut last_publish: Option<Instant> = None;

        // Last valid reading per sensor, kept for display when a read fails.
        let mut last_valid: Vec<Option<f32>> = vec![None; sensor_count];
        let mut last_energy: Option<f32> = None;

        loop {
            interval.tick().await;

            // Read all sensors in parallel on blocking threads: a DS18B20
            // conversion takes ~750 ms, so sequential reads of 6 sensors
            // would overrun the 2 s tick and stall the async runtime.
            let handles: Vec<_> = sensor_config
                .sensors
                .iter()
                .map(|s| {
                    let id = s.id.clone();
                    tokio::task::spawn_blocking(move || sensors::read_temperature(&id))
                })
                .collect();

            let mut readings: Vec<Option<f32>> = Vec::with_capacity(sensor_count);
            for (handle, sensor) in handles.into_iter().zip(&sensor_config.sensors) {
                let reading = match handle.await {
                    Ok(Ok(temp)) => Some(temp),
                    Ok(Err(e)) => {
                        warn!("Error reading sensor {}: {e:#}", sensor.name);
                        None
                    }
                    Err(e) => {
                        error!("Sensor read task failed for {}: {e}", sensor.name);
                        None
                    }
                };
                readings.push(reading);
            }
            for (last, reading) in last_valid.iter_mut().zip(&readings) {
                if reading.is_some() {
                    *last = *reading;
                }
            }

            // Energy is computed from the sensors that answered this tick,
            // so a failed sensor no longer drags the average down to 0 °C.
            let valid: Vec<f32> = readings.iter().flatten().copied().collect();
            let energy_now = energy::stored_energy_kwh(&valid, &sensor_config.boiler);
            if energy_now.is_some() {
                last_energy = energy_now;
            }

            // History point every 15 minutes (a failed sensor leaves a gap),
            // persisted so a restart doesn't wipe the graphs.
            let now = Instant::now();
            if now.duration_since(last_history_update) >= HISTORY_INTERVAL {
                for (sensor_history, reading) in hist.iter_mut().zip(&readings) {
                    sensor_history.add_point(*reading);
                }
                last_history_update = now;
                if let Err(e) = history::save(&sensor_config.history_file, &hist) {
                    warn!(
                        "Failed to persist history to {}: {e:#}",
                        sensor_config.history_file
                    );
                }
            }

            // Throttled, retained MQTT publishes. try_publish never blocks
            // the loop when the broker is unreachable and the queue fills up.
            if last_publish.is_none_or(|t| now.duration_since(t) >= publish_interval) {
                last_publish = Some(now);
                for (sensor, reading) in sensor_config.sensors.iter().zip(&readings) {
                    if let Some(temp) = reading {
                        let topic = format!("{}/{}", sensor_config.mqtt.base_topic, sensor.name);
                        let _ = client.try_publish(
                            topic,
                            rumqttc::QoS::AtLeastOnce,
                            true,
                            format!("{temp:.2}"),
                        );
                    }
                }
                if let Some(energy_kwh) = energy_now {
                    let topic = format!("{}/energy", sensor_config.mqtt.base_topic);
                    let _ = client.try_publish(
                        topic,
                        rumqttc::QoS::AtLeastOnce,
                        true,
                        format!("{energy_kwh:.3}"),
                    );
                }
            }

            // Prepare display data off the UI thread.
            let mut rows: Vec<SensorData> = Vec::with_capacity(sensor_count);
            for i in 0..sensor_count {
                let graph = hist[i].svg_graph();
                let (min_label, max_label) = if graph.line.is_empty() {
                    (String::new(), String::new())
                } else {
                    (format!("{:.0}°", graph.lo), format!("{:.0}°", graph.hi))
                };
                rows.push(SensorData {
                    name: sensor_config.sensors[i].name.clone().into(),
                    value_text: match last_valid[i] {
                        Some(temp) => format!("{temp:.1} °C").into(),
                        None => "--".into(),
                    },
                    ok: readings[i].is_some(),
                    history_path: graph.line.into(),
                    history_fill: graph.fill.into(),
                    hist_min_text: min_label.into(),
                    hist_max_text: max_label.into(),
                });
            }

            let energy_text: slint::SharedString = match last_energy {
                Some(energy_kwh) => format!("{energy_kwh:.1}").into(),
                None => "-.-".into(),
            };
            let all_sensors_ok = readings.iter().all(Option::is_some);
            let mqtt_ok = connected.load(Ordering::Relaxed);
            // Tank gradient stops, top (first sensor) to bottom (last sensor).
            let strat = stratification_colors(&last_valid);

            // Batch UI updates and send them to the main Slint thread.
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_sensors(slint::ModelRc::from(rows.as_slice()));
                    ui.set_energy_text(energy_text);
                    ui.set_mqtt_connected(mqtt_ok);
                    ui.set_sensors_ok(all_sensors_ok);
                    ui.set_boiler_c0(strat[0]);
                    ui.set_boiler_c1(strat[1]);
                    ui.set_boiler_c2(strat[2]);
                    ui.set_boiler_c3(strat[3]);
                    ui.set_boiler_c4(strat[4]);
                    ui.set_boiler_c5(strat[5]);
                }
            });
        }
    });

    // Start the Slint UI main loop
    ui.run()?;

    Ok(())
}

/// Marks the connection as lost and returns whether it was previously up.
fn mqtt_was_connected(connected: &AtomicBool) -> bool {
    connected.swap(false, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_covers_the_whole_tank() {
        // Sensors at the top, middle and bottom of the tank.
        let known = vec![(0.0, 60.0), (0.5, 40.0), (1.0, 20.0)];
        assert_eq!(interpolate_at(&known, 0.0), Some(60.0));
        assert_eq!(interpolate_at(&known, 0.25), Some(50.0));
        assert_eq!(interpolate_at(&known, 1.0), Some(20.0));
    }

    #[test]
    fn interpolation_extends_to_edges_when_sensors_fail() {
        // Only the two middle sensors are valid.
        let known = vec![(0.4, 50.0), (0.6, 30.0)];
        assert_eq!(interpolate_at(&known, 0.0), Some(50.0));
        assert_eq!(interpolate_at(&known, 0.5), Some(40.0));
        assert_eq!(interpolate_at(&known, 1.0), Some(30.0));
    }

    #[test]
    fn no_reading_gives_no_temperature() {
        assert_eq!(interpolate_at(&[], 0.5), None);
    }
}
