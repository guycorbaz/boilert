//! Thermal energy calculation for the stratified tank.

use crate::config::BoilerConfig;

/// Computes the thermal energy stored above the reference temperature, in kWh.
///
/// Each temperature is assumed to represent an equal horizontal slice of the
/// cylindrical tank (sensors evenly spaced along its height). Slices colder
/// than the reference temperature contribute zero rather than a negative
/// amount, so a single cold slice cannot hide heat stored higher up.
///
/// Returns `None` when no valid temperature is available.
pub fn stored_energy_kwh(temps: &[f32], boiler: &BoilerConfig) -> Option<f32> {
    if temps.is_empty() {
        return None;
    }
    let slice_volume_l = boiler.volume_l / temps.len() as f32;
    let wh: f32 = temps
        .iter()
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
    fn no_sensors_gives_none() {
        assert!(stored_energy_kwh(&[], &boiler()).is_none());
    }

    #[test]
    fn uniform_temperature_matches_reference_formula() {
        // 500 l heated from 15 °C to 55 °C: 500 * 40 * 1.162 / 1000 = 23.24 kWh
        let energy = stored_energy_kwh(&[55.0, 55.0, 55.0], &boiler()).unwrap();
        assert!((energy - 23.24).abs() < 1e-3);
    }

    #[test]
    fn cold_slices_do_not_subtract_energy() {
        // Top slice hot, bottom slice below the reference temperature:
        // only the hot slice counts (250 l * 40 K * 1.162 / 1000).
        let energy = stored_energy_kwh(&[55.0, 10.0], &boiler()).unwrap();
        assert!((energy - 11.62).abs() < 1e-3);
    }

    #[test]
    fn stratified_tank_matches_slice_sum() {
        // 2 slices of 250 l: (60-15) and (30-15) K.
        let energy = stored_energy_kwh(&[60.0, 30.0], &boiler()).unwrap();
        let expected = (250.0 * 45.0 * 1.162 + 250.0 * 15.0 * 1.162) / 1000.0;
        assert!((energy - expected).abs() < 1e-3);
    }
}
