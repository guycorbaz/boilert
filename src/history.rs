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
/// Width of the SVG viewbox of the sensor card graphs. Slint scales a Path
/// viewbox preserving its aspect ratio, so this matches the ~6:1 aspect of
/// the graph area in the sensor cards (height is always 100).
pub const SENSOR_GRAPH_VIEW_W: f32 = 600.0;
/// Width of the SVG viewbox of the energy graph on the dashboard (matches
/// the 320x100 px graph area of the energy card).
pub const ENERGY_GRAPH_VIEW_W: f32 = 320.0;

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
    /// The X axis spans 0..=`view_w`. The Y axis is auto-scaled to the data
    /// (with a margin and at least a `min_span` range) and mapped to the
    /// 0..100 viewbox, 0 being the highest bound at the top.
    /// Gaps lift the pen, so sensor failures don't draw misleading lines.
    pub fn svg_graph(&self, view_w: f32, min_span: f32) -> SvgGraph {
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
        // Margin on each side, and at least a `min_span` range so that a
        // nearly flat curve isn't magnified into dramatic swings.
        let margin = min_span / 5.0;
        let mut lo = min - margin;
        let mut hi = max + margin;
        if hi - lo < min_span {
            let center = (hi + lo) / 2.0;
            lo = center - min_span / 2.0;
            hi = center + min_span / 2.0;
        }

        // `line` traces the curve; `fill` closes each contiguous segment down
        // to the baseline (y = 100) so the area under the curve can be filled.
        let x_step = view_w / (HISTORY_POINTS - 1) as f32;
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

/// Full application history: one buffer per sensor plus the stored energy.
/// This is also the on-disk (JSON) representation.
#[derive(Serialize, Deserialize)]
pub struct HistoryStore {
    /// Unix timestamp (seconds) of the last save.
    saved_at: u64,
    /// One history per configured sensor, in configuration order.
    pub sensors: Vec<SensorHistory>,
    /// History of the total stored energy (kWh).
    #[serde(default = "SensorHistory::new")]
    pub energy: SensorHistory,
}

impl HistoryStore {
    fn new(sensor_count: usize) -> Self {
        Self {
            saved_at: 0,
            sensors: vec![SensorHistory::new(); sensor_count],
            energy: SensorHistory::new(),
        }
    }

    /// Appends `points` gaps to every buffer so older points keep their time
    /// slot after downtime or a forward wall-clock jump.
    pub fn shift(&mut self, points: usize) {
        for history in self.buffers() {
            for _ in 0..points.min(HISTORY_POINTS) {
                history.add_point(None);
            }
        }
    }

    fn buffers(&mut self) -> impl Iterator<Item = &mut SensorHistory> {
        self.sensors
            .iter_mut()
            .chain(std::iter::once(&mut self.energy))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Restores the persisted history, or starts fresh if the file is missing,
/// unreadable, or no longer matches the configured sensors.
pub fn load_or_new(path: &str, sensor_count: usize) -> HistoryStore {
    if !Path::new(path).exists() {
        tracing::info!("No history file at {path}, starting fresh");
        return HistoryStore::new(sensor_count);
    }
    match load(path, sensor_count) {
        Ok(store) => {
            tracing::info!("Restored temperature history from {path}");
            store
        }
        Err(e) => {
            tracing::warn!("Could not restore history from {path}: {e:#}");
            HistoryStore::new(sensor_count)
        }
    }
}

fn load(path: &str, sensor_count: usize) -> Result<HistoryStore> {
    let content = fs::read_to_string(path).context("read failed")?;
    let mut store: HistoryStore = serde_json::from_str(&content).context("parse failed")?;
    ensure!(
        store.sensors.len() == sensor_count,
        "sensor count changed ({} in file, {} configured)",
        store.sensors.len(),
        sensor_count
    );
    for history in store.buffers() {
        ensure!(
            history.points.len() == HISTORY_POINTS,
            "unexpected point count in history file"
        );
    }
    // Shift the buffers by the downtime so old points keep their time slot.
    // At boot on a Pi without an RTC the clock can still be behind (NTP not
    // yet synced): saturating_sub then yields 0 and the forward clock jump
    // is compensated later by the jump detection in the main loop.
    let missed = (unix_now().saturating_sub(store.saved_at) / POINT_INTERVAL_S) as usize;
    store.shift(missed);
    Ok(store)
}

/// Saves the history atomically and durably: write to a temp file, fsync it,
/// rename it over the target, then fsync the parent directory. Without the
/// fsyncs, a power cut (the normal way a wall-mounted kiosk is turned off)
/// could leave an empty file behind the already-visible rename.
pub fn save(path: &str, store: &HistoryStore) -> Result<()> {
    use std::io::Write;

    let file = HistoryStore {
        saved_at: unix_now(),
        sensors: store.sensors.clone(),
        energy: store.energy.clone(),
    };
    let json = serde_json::to_string(&file).context("serialize failed")?;
    let tmp = format!("{path}.tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("failed to create {tmp}"))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("failed to write {tmp}"))?;
        f.sync_all().with_context(|| format!("failed to sync {tmp}"))?;
    }
    fs::rename(&tmp, path).with_context(|| format!("failed to rename {tmp} to {path}"))?;
    let dir = match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("failed to sync directory {}", dir.display()))?;
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
        let graph = SensorHistory::new().svg_graph(600.0, 5.0);
        assert!(graph.line.is_empty());
        assert!(graph.fill.is_empty());
    }

    #[test]
    fn autoscale_enforces_minimum_span() {
        let mut history = SensorHistory::new();
        history.add_point(Some(50.0));
        history.add_point(Some(50.2));
        let graph = history.svg_graph(600.0, 5.0);
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
        let graph = history.svg_graph(600.0, 5.0);
        // Two separate sub-paths: one before the gap, one after.
        assert_eq!(graph.line.matches('M').count(), 2);
        // The fill closes one area per contiguous segment.
        assert_eq!(graph.fill.matches('Z').count(), 2);
        // Each fill area starts and ends on the baseline (y = 100).
        assert_eq!(graph.fill.matches("100 ").count(), 4);
    }

    #[test]
    fn shift_moves_points_back_and_pads_with_gaps() {
        let mut store = HistoryStore::new(1);
        store.sensors[0].add_point(Some(50.0));
        store.energy.add_point(Some(10.0));
        store.shift(2);
        let n = HISTORY_POINTS;
        assert_eq!(store.sensors[0].points[n - 3], Some(50.0));
        assert_eq!(store.sensors[0].points[n - 1], None);
        assert_eq!(store.energy.points[n - 3], Some(10.0));
        // A huge shift is capped at the buffer size and empties it.
        store.shift(usize::MAX);
        assert!(store.sensors[0].points.iter().all(Option::is_none));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("boilert-history-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let path = path.to_str().unwrap();

        let mut store = HistoryStore::new(2);
        store.sensors[0].add_point(Some(55.5));
        store.sensors[1].add_point(Some(22.2));
        store.energy.add_point(Some(12.3));
        save(path, &store).unwrap();

        let restored = load(path, 2).unwrap();
        assert_eq!(restored.sensors[0].points.last().copied().flatten(), Some(55.5));
        assert_eq!(restored.sensors[1].points.last().copied().flatten(), Some(22.2));
        assert_eq!(restored.energy.points.last().copied().flatten(), Some(12.3));

        // A different sensor count must be rejected.
        assert!(load(path, 3).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn load_accepts_files_without_energy_history() {
        // Files written by older versions have no `energy` field.
        let dir = std::env::temp_dir().join("boilert-history-test-compat");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let path = path.to_str().unwrap();

        let store = HistoryStore::new(1);
        let mut json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("energy");
        std::fs::write(path, serde_json::to_string(&json).unwrap()).unwrap();

        let restored = load(path, 1).unwrap();
        assert_eq!(restored.energy.points.len(), HISTORY_POINTS);
        std::fs::remove_file(path).unwrap();
    }
}
