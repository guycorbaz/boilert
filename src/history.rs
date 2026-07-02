//! 24-hour temperature history (15-minute resolution) with disk persistence.
//!
//! Each sensor keeps a rolling buffer of [`HISTORY_POINTS`] optional values;
//! `None` marks a gap (failed reading or downtime between two runs). The whole
//! history is saved to a JSON file after every new point and restored at
//! startup, shifted by the time the application was not running.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of points kept per sensor: 24 hours at 4 points per hour.
pub const HISTORY_POINTS: usize = 96;
/// Seconds between two history points (15 minutes).
pub const POINT_INTERVAL_S: u64 = 15 * 60;

/// Rolling buffer of temperature values for a single sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorHistory {
    /// Oldest point first; `None` marks a gap in the data.
    points: Vec<Option<f32>>,
}

impl SensorHistory {
    /// Creates an empty history (all gaps).
    pub fn new() -> Self {
        Self {
            points: vec![None; HISTORY_POINTS],
        }
    }

    /// Pushes a new point, dropping the oldest one.
    pub fn add_point(&mut self, val: Option<f32>) {
        self.points.remove(0);
        self.points.push(val);
    }

    /// Maps the history to an SVG path for Slint's `Path` element, along with
    /// the y-axis bounds `(path, min_temp, max_temp)` used for the scaling.
    ///
    /// The X axis ranges over 0..=95 (`HISTORY_POINTS - 1`). The Y axis is
    /// auto-scaled to the data (with a margin and a minimum 5 °C span) and
    /// mapped to the 0..100 viewbox, 0 being the hottest bound at the top.
    /// Gaps lift the pen, so sensor failures don't draw misleading lines.
    pub fn svg_path(&self) -> (String, f32, f32) {
        let valid: Vec<f32> = self.points.iter().flatten().copied().collect();
        if valid.is_empty() {
            return (String::new(), 0.0, 100.0);
        }
        let min = valid.iter().copied().fold(f32::INFINITY, f32::min);
        let max = valid.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        // 1 °C margin on each side, and at least a 5 °C span so that a nearly
        // flat curve isn't magnified into dramatic swings.
        let mut lo = min - 1.0;
        let mut hi = max + 1.0;
        if hi - lo < 5.0 {
            let center = (hi + lo) / 2.0;
            lo = center - 2.5;
            hi = center + 2.5;
        }

        let mut path = String::new();
        let mut pen_down = false;
        for (i, point) in self.points.iter().enumerate() {
            match point {
                Some(temp) => {
                    let y = ((hi - temp) / (hi - lo) * 100.0).clamp(0.0, 100.0);
                    let cmd = if pen_down { 'L' } else { 'M' };
                    path.push_str(&format!("{cmd} {i} {y:.1} "));
                    pen_down = true;
                }
                None => pen_down = false,
            }
        }
        (path, lo, hi)
    }
}

/// On-disk representation of the history of all sensors.
#[derive(Serialize, Deserialize)]
struct HistoryFile {
    /// Unix timestamp (seconds) of the last save.
    saved_at: u64,
    sensors: Vec<SensorHistory>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Restores the persisted history, or starts fresh if the file is missing,
/// unreadable, or no longer matches the configured sensors.
pub fn load_or_new(path: &str, sensor_count: usize) -> Vec<SensorHistory> {
    if !Path::new(path).exists() {
        tracing::info!("No history file at {path}, starting fresh");
        return vec![SensorHistory::new(); sensor_count];
    }
    match load(path, sensor_count) {
        Ok(history) => {
            tracing::info!("Restored temperature history from {path}");
            history
        }
        Err(e) => {
            tracing::warn!("Could not restore history from {path}: {e:#}");
            vec![SensorHistory::new(); sensor_count]
        }
    }
}

fn load(path: &str, sensor_count: usize) -> Result<Vec<SensorHistory>> {
    let content = fs::read_to_string(path).context("read failed")?;
    let file: HistoryFile = serde_json::from_str(&content).context("parse failed")?;
    ensure!(
        file.sensors.len() == sensor_count,
        "sensor count changed ({} in file, {} configured)",
        file.sensors.len(),
        sensor_count
    );
    // Shift the buffers by the downtime so old points keep their time slot.
    let missed = (unix_now().saturating_sub(file.saved_at) / POINT_INTERVAL_S) as usize;
    let mut sensors = file.sensors;
    for history in &mut sensors {
        ensure!(
            history.points.len() == HISTORY_POINTS,
            "unexpected point count in history file"
        );
        for _ in 0..missed.min(HISTORY_POINTS) {
            history.add_point(None);
        }
    }
    Ok(sensors)
}

/// Saves the history atomically (write to a temp file, then rename).
pub fn save(path: &str, sensors: &[SensorHistory]) -> Result<()> {
    let file = HistoryFile {
        saved_at: unix_now(),
        sensors: sensors.to_vec(),
    };
    let json = serde_json::to_string(&file).context("serialize failed")?;
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, json).with_context(|| format!("failed to write {tmp}"))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to rename {tmp} to {path}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_point_keeps_buffer_size_and_order() {
        let mut history = SensorHistory::new();
        history.add_point(Some(42.0));
        assert_eq!(history.points.len(), HISTORY_POINTS);
        assert_eq!(history.points.last().copied().flatten(), Some(42.0));
        assert_eq!(history.points.first().copied().flatten(), None);
    }

    #[test]
    fn empty_history_gives_empty_path() {
        let (path, _, _) = SensorHistory::new().svg_path();
        assert!(path.is_empty());
    }

    #[test]
    fn autoscale_enforces_minimum_span() {
        let mut history = SensorHistory::new();
        history.add_point(Some(50.0));
        history.add_point(Some(50.2));
        let (path, lo, hi) = history.svg_path();
        assert!(!path.is_empty());
        assert!((hi - lo - 5.0).abs() < 1e-3);
        assert!(lo < 50.0 && hi > 50.2);
    }

    #[test]
    fn gaps_lift_the_pen() {
        let mut history = SensorHistory::new();
        history.add_point(Some(40.0));
        history.add_point(Some(41.0));
        history.add_point(None);
        history.add_point(Some(42.0));
        history.add_point(Some(43.0));
        let (path, _, _) = history.svg_path();
        // Two separate sub-paths: one before the gap, one after.
        assert_eq!(path.matches('M').count(), 2);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("boilert-history-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let path = path.to_str().unwrap();

        let mut history = vec![SensorHistory::new(), SensorHistory::new()];
        history[0].add_point(Some(55.5));
        history[1].add_point(Some(22.2));
        save(path, &history).unwrap();

        let restored = load(path, 2).unwrap();
        assert_eq!(restored[0].points.last().copied().flatten(), Some(55.5));
        assert_eq!(restored[1].points.last().copied().flatten(), Some(22.2));

        // A different sensor count must be rejected.
        assert!(load(path, 3).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
