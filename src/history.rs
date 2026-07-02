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
/// Width of the SVG viewbox the graph is rendered into. Slint scales a Path
/// viewbox preserving its aspect ratio, so this matches the ~6:1 aspect of
/// the graph area in the sensor cards (height is 100).
pub const GRAPH_VIEW_W: f32 = 600.0;

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

    /// Maps the history to SVG paths for Slint's `Path` elements, auto-scaled
    /// to the data.
    ///
    /// The X axis spans 0..=[`GRAPH_VIEW_W`]. The Y axis is auto-scaled to
    /// the data (with a margin and a minimum 5 °C span) and mapped to the
    /// 0..100 viewbox, 0 being the hottest bound at the top.
    /// Gaps lift the pen, so sensor failures don't draw misleading lines.
    pub fn svg_graph(&self) -> SvgGraph {
        let valid: Vec<f32> = self.points.iter().flatten().copied().collect();
        if valid.is_empty() {
            return SvgGraph {
                line: String::new(),
                fill: String::new(),
                lo: 0.0,
                hi: 100.0,
            };
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

        // `line` traces the curve; `fill` closes each contiguous segment down
        // to the baseline (y = 100) so the area under the curve can be filled.
        let x_step = GRAPH_VIEW_W / (HISTORY_POINTS - 1) as f32;
        let mut line = String::new();
        let mut fill = String::new();
        let mut segment_last: Option<f32> = None;
        for (i, point) in self.points.iter().enumerate() {
            let x = i as f32 * x_step;
            match point {
                Some(temp) => {
                    let y = ((hi - temp) / (hi - lo) * 100.0).clamp(0.0, 100.0);
                    if segment_last.is_none() {
                        line.push_str(&format!("M {x:.1} {y:.1} "));
                        fill.push_str(&format!("M {x:.1} 100 L {x:.1} {y:.1} "));
                    } else {
                        line.push_str(&format!("L {x:.1} {y:.1} "));
                        fill.push_str(&format!("L {x:.1} {y:.1} "));
                    }
                    segment_last = Some(x);
                }
                None => {
                    if let Some(last) = segment_last.take() {
                        fill.push_str(&format!("L {last:.1} 100 Z "));
                    }
                }
            }
        }
        if let Some(last) = segment_last {
            fill.push_str(&format!("L {last:.1} 100 Z "));
        }
        SvgGraph { line, fill, lo, hi }
    }
}

/// SVG paths and y-axis bounds of a history graph.
pub struct SvgGraph {
    /// Path commands of the temperature curve.
    pub line: String,
    /// Path commands of the closed area under the curve (for a gradient fill).
    pub fill: String,
    /// Lower bound of the auto-scaled y axis (°C).
    pub lo: f32,
    /// Upper bound of the auto-scaled y axis (°C).
    pub hi: f32,
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
    fn empty_history_gives_empty_paths() {
        let graph = SensorHistory::new().svg_graph();
        assert!(graph.line.is_empty());
        assert!(graph.fill.is_empty());
    }

    #[test]
    fn autoscale_enforces_minimum_span() {
        let mut history = SensorHistory::new();
        history.add_point(Some(50.0));
        history.add_point(Some(50.2));
        let graph = history.svg_graph();
        assert!(!graph.line.is_empty());
        assert!((graph.hi - graph.lo - 5.0).abs() < 1e-3);
        assert!(graph.lo < 50.0 && graph.hi > 50.2);
    }

    #[test]
    fn gaps_lift_the_pen() {
        let mut history = SensorHistory::new();
        history.add_point(Some(40.0));
        history.add_point(Some(41.0));
        history.add_point(None);
        history.add_point(Some(42.0));
        history.add_point(Some(43.0));
        let graph = history.svg_graph();
        // Two separate sub-paths: one before the gap, one after.
        assert_eq!(graph.line.matches('M').count(), 2);
        // The fill closes one area per contiguous segment.
        assert_eq!(graph.fill.matches('Z').count(), 2);
        // Each fill area starts and ends on the baseline (y = 100).
        assert_eq!(graph.fill.matches("100 ").count(), 4);
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
