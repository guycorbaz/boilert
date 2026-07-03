//! Thermal energy calculation for the stratified tank, and the temperature
//! profile helpers shared with the stratification display.

use crate::config::BoilerConfig;

/// Relative height of sensor `i` among `n` sensors (0.0 = top, 1.0 = bottom).
fn sensor_position(i: usize, n: usize) -> f32 {
    if n > 1 { i as f32 / (n - 1) as f32 } else { 0.5 }
}

/// Extracts the valid (relative position, temperature) pairs of the measured
/// profile, ordered from the top of the tank (0.0) to the bottom (1.0).
/// The first configured sensor is at the top of the tank.
pub fn known_profile(last_valid: &[Option<f32>]) -> Vec<(f32, f32)> {
    let n = last_valid.len();
    last_valid
        .iter()
        .enumerate()
        .filter_map(|(i, temp)| temp.map(|t| (sensor_position(i, n), t)))
        .collect()
}

/// Interpolates the temperature at relative tank height `p` (0.0 = top,
/// 1.0 = bottom) from the valid sensor readings, or `None` if there are none.
pub fn interpolate_at(known: &[(f32, f32)], p: f32) -> Option<f32> {
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

/// Computes the thermal energy stored above the reference temperature, in kWh.
///
/// `temps` holds the last known temperature of each configured sensor, from
/// the TOP of the tank to the BOTTOM; each sensor represents an equal
/// horizontal slice of the cylindrical tank. The slice of a sensor without a
/// valid reading is interpolated from its neighbors (clamped at the ends) —
/// the same profile the dashboard draws — so the slice weighting stays
/// correct when sensors fail and the energy figure matches the display.
///
/// Slices colder than the reference temperature contribute zero rather than
/// a negative amount, so a single cold slice cannot hide heat stored higher
/// up. Returns `None` when no sensor has a valid temperature at all.
pub fn stored_energy_kwh(temps: &[Option<f32>], boiler: &BoilerConfig) -> Option<f32> {
    let known = known_profile(temps);
    if known.is_empty() {
        return None;
    }
    let n = temps.len();
    let slice_volume_l = boiler.volume_l / n as f32;
    let wh: f32 = (0..n)
        .filter_map(|i| interpolate_at(&known, sensor_position(i, n)))
        .map(|t| (t - boiler.reference_temp_c).max(0.0) * slice_volume_l * boiler.energy_coefficient)
        .sum();
    Some(wh / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boiler() -> BoilerConfig {
        BoilerConfig {
            volume_l: 500.0,
            reference_temp_c: 15.0,
            energy_coefficient: 1.162,
        }
    }

    #[test]
    fn no_valid_sensor_gives_none() {
        assert!(stored_energy_kwh(&[], &boiler()).is_none());
        assert!(stored_energy_kwh(&[None, None], &boiler()).is_none());
    }

    #[test]
    fn uniform_temperature_matches_reference_formula() {
        // 500 l heated from 15 °C to 55 °C: 500 * 40 * 1.162 / 1000 = 23.24 kWh
        let energy =
            stored_energy_kwh(&[Some(55.0), Some(55.0), Some(55.0)], &boiler()).unwrap();
        assert!((energy - 23.24).abs() < 1e-3);
    }

    #[test]
    fn cold_slices_do_not_subtract_energy() {
        // Top slice hot, bottom slice below the reference temperature:
        // only the hot slice counts (250 l * 40 K * 1.162 / 1000).
        let energy = stored_energy_kwh(&[Some(55.0), Some(10.0)], &boiler()).unwrap();
        assert!((energy - 11.62).abs() < 1e-3);
    }

    #[test]
    fn stratified_tank_matches_slice_sum() {
        // 2 slices of 250 l: (60-15) and (30-15) K.
        let energy = stored_energy_kwh(&[Some(60.0), Some(30.0)], &boiler()).unwrap();
        let expected = (250.0 * 45.0 * 1.162 + 250.0 * 15.0 * 1.162) / 1000.0;
        assert!((energy - expected).abs() < 1e-3);
    }

    #[test]
    fn failed_sensor_slice_is_interpolated_not_redistributed() {
        // 3 slices, middle sensor failed: its slice is the average of its
        // neighbors (45 °C), not a redistribution of the volume over the
        // two valid sensors.
        let energy = stored_energy_kwh(&[Some(60.0), None, Some(30.0)], &boiler()).unwrap();
        let slice = 500.0 / 3.0;
        let expected = (slice * 45.0 + slice * 30.0 + slice * 15.0) * 1.162 / 1000.0;
        assert!((energy - expected).abs() < 1e-3);
    }

    #[test]
    fn edge_slices_clamp_to_nearest_valid_sensor() {
        // Only the middle sensor of three is valid: the whole tank is
        // assumed at its temperature, consistently with the tank drawing.
        let energy = stored_energy_kwh(&[None, Some(40.0), None], &boiler()).unwrap();
        let expected = 500.0 * 25.0 * 1.162 / 1000.0;
        assert!((energy - expected).abs() < 1e-3);
    }

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

    #[test]
    fn known_profile_maps_indices_to_relative_heights() {
        let profile = known_profile(&[Some(60.0), None, Some(20.0)]);
        assert_eq!(profile, vec![(0.0, 60.0), (1.0, 20.0)]);
        // A single sensor sits at mid-height.
        assert_eq!(known_profile(&[Some(50.0)]), vec![(0.5, 50.0)]);
    }
}
