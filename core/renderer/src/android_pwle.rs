/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::time::Duration;

use haptic_data::Breakpoint;
use haptic_data::HapticData;
use haptic_data::interpolate_breakpoints;
use haptic_data::v1::FrequencyBreakpoint;
use serde::Deserialize;
use serde::Serialize;
use typeshare::typeshare;

use crate::android_waveform::ANDROID_ACF;
use crate::error::Error;

/// A single PWLE control point for the Android VibrationEffect.BasicEnvelopeBuilder API
///
/// Each control point defines an amplitude, frequency, and the duration from the previous control
/// point to this one. The vibrator linearly interpolates between consecutive control points.
pub struct PwleControlPoint {
    /// The amplitude of the vibration at this control point, in the range [0.0, 1.0]
    pub amplitude: f32,
    /// The frequency of the vibration at this control point, in the range [0.0, 1.0]
    pub frequency: f32,
    /// The duration from the previous control point to this one
    pub duration: Duration,
}

/// A transient (emphasis) event extracted from the haptic data
///
/// Instead of being emulated as amplitude curve spikes in the control points, transients are
/// output separately so the player can render them using vibration primitives (e.g.
/// `VibrationEffect.Composition.PRIMITIVE_CLICK`).
pub struct PwleTransient {
    /// The intensity of the transient, in the range [0.0, 1.0]
    pub amplitude: f32,
    /// The absolute time of the transient within the clip
    pub time: Duration,
}

impl PwleTransient {
    /// The absolute time of the transient in whole milliseconds, rounded to the nearest one
    pub fn time_ms(&self) -> u64 {
        duration_to_rounded_millis(self.time)
    }
}

/// The result of rendering haptic data into PWLE format
///
/// Contains the continuous envelope as control points (for `BasicEnvelopeBuilder`) and the
/// transient events as a separate list (for `VibrationEffect.Composition` primitives).
pub struct PwleOutput {
    /// The continuous envelope control points
    pub control_points: Vec<PwleControlPoint>,
    /// The transient (emphasis) events, sorted by time
    pub transients: Vec<PwleTransient>,
}

impl PwleOutput {
    /// The duration of each control point from the previous one, in whole milliseconds
    ///
    /// The rounding is done on the cumulative timeline instead of on each duration, so that the
    /// rounding errors don't accumulate and the control points stay aligned with the transients.
    pub fn control_point_durations_ms(&self) -> Vec<u64> {
        let mut elapsed = Duration::ZERO;
        let mut previous_ms = 0;
        self.control_points
            .iter()
            .map(|point| {
                elapsed += point.duration;
                let elapsed_ms = duration_to_rounded_millis(elapsed);
                let duration_ms = elapsed_ms - previous_ms;
                previous_ms = elapsed_ms;
                duration_ms
            })
            .collect()
    }
}

fn duration_to_rounded_millis(duration: Duration) -> u64 {
    (duration.as_secs_f64() * 1000.0).round() as u64
}

fn duration_from_secs(seconds: f32) -> std::result::Result<Duration, Error> {
    Duration::try_from_secs_f32(seconds)
        .map_err(|_| Error::InvalidHapticData(format!("Invalid time in seconds: {seconds}")))
}

/// Settings for rendering haptic data into PWLE control points
#[derive(Clone, Copy, Deserialize, Serialize)]
#[typeshare]
pub struct PwleRenderSettings {
    /// The gain to apply to the amplitude values
    pub gain: f32,
}

impl Default for PwleRenderSettings {
    fn default() -> Self {
        Self {
            gain: ANDROID_ACF.continuous.gain,
        }
    }
}

/// Interpolates the frequency value from the frequency envelope at a given time
fn interpolate_frequency(frequency_envelope: Option<&[FrequencyBreakpoint]>, time: f32) -> f32 {
    match frequency_envelope {
        None | Some([]) => 0.0,
        Some([only]) => only.frequency,
        Some(freqs @ [first, .., last]) => {
            if time <= first.time {
                return first.frequency;
            }
            if time >= last.time {
                return last.frequency;
            }
            for window in freqs.windows(2) {
                let (a, b) = (&window[0], &window[1]);
                if time >= a.time() && time <= b.time() {
                    return interpolate_breakpoints(a, b, time).frequency;
                }
            }
            last.frequency
        }
    }
}

/// Renders haptic data into PWLE control points and transients
///
/// Walks through the amplitude envelope breakpoints and outputs them as PWLE control points with
/// interpolated frequency values. Transient (emphasis) events are collected separately rather than
/// being emulated as amplitude curve spikes.
pub fn render_pwle(
    haptic_data: &HapticData,
    render_settings: PwleRenderSettings,
) -> std::result::Result<PwleOutput, Error> {
    let amp_envelope = &haptic_data.signals.continuous.envelopes.amplitude;
    let freq_envelope = &haptic_data.signals.continuous.envelopes.frequency;

    if amp_envelope.len() < 2 {
        return Err(Error::InvalidHapticData(
            "Not enough amplitude points".to_string(),
        ));
    }

    let gain = render_settings.gain;

    struct TimedPoint {
        time: f32,
        amplitude: f32,
        frequency: f32,
    }

    let mut points: Vec<TimedPoint> = Vec::new();
    let mut transients: Vec<PwleTransient> = Vec::new();

    for bp in amp_envelope.iter() {
        let time = bp.time;
        let freq = interpolate_frequency(freq_envelope.as_deref(), time);
        let continuous_amplitude = (bp.amplitude * gain).clamp(0.0, 1.0);

        points.push(TimedPoint {
            time,
            amplitude: continuous_amplitude,
            frequency: freq,
        });

        if let Some(emphasis) = &bp.emphasis {
            transients.push(PwleTransient {
                amplitude: (emphasis.amplitude * gain).clamp(0.0, 1.0),
                time: duration_from_secs(time)?,
            });
        }
    }

    // Ensure the envelope ends at 0 amplitude as required by the PWLE spec
    if let Some(last) = points.last()
        && last.amplitude > f32::EPSILON
    {
        points.push(TimedPoint {
            time: last.time,
            amplitude: 0.0,
            frequency: last.frequency,
        });
    }

    // Convert timed points to PWLE control points with durations
    let control_points = points
        .iter()
        .enumerate()
        .map(|(i, point)| {
            let duration = match i.checked_sub(1) {
                None => Ok(Duration::ZERO),
                Some(prev) => duration_from_secs((point.time - points[prev].time).max(0.0)),
            };
            duration.map(|duration| PwleControlPoint {
                amplitude: point.amplitude,
                frequency: point.frequency,
                duration,
            })
        })
        .collect::<std::result::Result<Vec<_>, Error>>()?;

    Ok(PwleOutput {
        control_points,
        transients,
    })
}

#[cfg(test)]
mod tests {
    use assert_approx_eq::assert_approx_eq;
    use haptic_data::test_utils::TestClip;
    use haptic_data::test_utils::amp_bp;
    use haptic_data::test_utils::emphasis_bp;
    use haptic_data::test_utils::freq_bp;

    use super::*;

    fn assert_duration_approx_eq(actual: Duration, expected_ms: f32) {
        assert_approx_eq!(actual.as_secs_f32() * 1000.0, expected_ms, 0.01);
    }

    #[test]
    fn simple_ramp() {
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.0), amp_bp(0.05, 1.0)],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings::default()).unwrap();
        let points = &output.control_points;

        assert_eq!(points.len(), 3);
        assert_approx_eq!(points[0].amplitude, 0.0);
        assert_approx_eq!(points[0].frequency, 0.0);
        assert_duration_approx_eq(points[0].duration, 0.0);

        assert_approx_eq!(points[1].amplitude, 1.0);
        assert_approx_eq!(points[1].frequency, 0.0);
        assert_duration_approx_eq(points[1].duration, 50.0);

        // Trailing zero required by PWLE spec
        assert_approx_eq!(points[2].amplitude, 0.0);
        assert_duration_approx_eq(points[2].duration, 0.0);

        assert!(output.transients.is_empty());
    }

    #[test]
    fn with_frequency_envelope() {
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.0), amp_bp(0.5, 0.5), amp_bp(1.0, 1.0)],
            frequency: &[freq_bp(0.0, 0.0), freq_bp(1.0, 1.0)],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings::default()).unwrap();
        let points = &output.control_points;

        assert_eq!(points.len(), 4);

        assert_approx_eq!(points[0].amplitude, 0.0);
        assert_approx_eq!(points[0].frequency, 0.0);

        assert_approx_eq!(points[1].amplitude, 0.5);
        assert_approx_eq!(points[1].frequency, 0.5);
        assert_duration_approx_eq(points[1].duration, 500.0);

        assert_approx_eq!(points[2].amplitude, 1.0);
        assert_approx_eq!(points[2].frequency, 1.0);
        assert_duration_approx_eq(points[2].duration, 500.0);

        // Trailing zero required by PWLE spec
        assert_approx_eq!(points[3].amplitude, 0.0);
        assert_duration_approx_eq(points[3].duration, 0.0);

        assert!(output.transients.is_empty());
    }

    #[test]
    fn gain_scales_amplitude() {
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.5), amp_bp(1.0, 1.0)],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings { gain: 0.5 }).unwrap();
        let points = &output.control_points;

        assert_eq!(points.len(), 3);
        assert_approx_eq!(points[0].amplitude, 0.25);
        assert_approx_eq!(points[1].amplitude, 0.5);
        // Trailing zero required by PWLE spec
        assert_approx_eq!(points[2].amplitude, 0.0);
    }

    #[test]
    fn gain_clamps_amplitude() {
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.5), amp_bp(1.0, 1.0)],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings { gain: 1.5 }).unwrap();
        let points = &output.control_points;

        assert_eq!(points.len(), 3);
        assert_approx_eq!(points[0].amplitude, 0.75);
        assert_approx_eq!(points[1].amplitude, 1.0);
        // Trailing zero required by PWLE spec
        assert_approx_eq!(points[2].amplitude, 0.0);
    }

    #[test]
    fn emphasis_produces_separate_transients() {
        let clip = TestClip {
            amplitude: &[
                amp_bp(0.0, 0.5),
                emphasis_bp(0.5, 0.5, 1.0, 0.0),
                amp_bp(1.0, 0.5),
            ],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings::default()).unwrap();
        let points = &output.control_points;

        // 3 continuous breakpoints + 1 trailing zero = 4 control points
        // (no extra points inserted for emphasis)
        assert_eq!(points.len(), 4);
        assert_approx_eq!(points[0].amplitude, 0.5);
        assert_approx_eq!(points[1].amplitude, 0.5);
        assert_approx_eq!(points[2].amplitude, 0.5);
        assert_approx_eq!(points[3].amplitude, 0.0);

        // Transient is collected separately
        assert_eq!(output.transients.len(), 1);
        assert_approx_eq!(output.transients[0].amplitude, 1.0);
        assert_duration_approx_eq(output.transients[0].time, 500.0);
    }

    #[test]
    fn gain_scales_transient_amplitude() {
        let clip = TestClip {
            amplitude: &[
                amp_bp(0.0, 0.5),
                emphasis_bp(0.5, 0.5, 0.8, 0.0),
                amp_bp(1.0, 0.5),
            ],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings { gain: 0.5 }).unwrap();

        assert_eq!(output.transients.len(), 1);
        assert_approx_eq!(output.transients[0].amplitude, 0.4);
    }

    #[test]
    fn ends_at_zero_amplitude() {
        // If the envelope already ends at 0, no extra point is added
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.0), amp_bp(0.5, 1.0), amp_bp(1.0, 0.0)],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings::default()).unwrap();
        let points = &output.control_points;

        assert_eq!(points.len(), 3);
        assert_approx_eq!(points[0].amplitude, 0.0);
        assert_approx_eq!(points[1].amplitude, 1.0);
        assert_approx_eq!(points[2].amplitude, 0.0);
    }

    #[test]
    fn durations_ms_do_not_accumulate_rounding_errors() {
        // Every segment lasts 1.4 ms, which would truncate to 1 ms and drift by 0.4 ms per segment
        let clip = TestClip {
            amplitude: &[
                amp_bp(0.0, 0.0),
                amp_bp(0.0014, 0.5),
                amp_bp(0.0028, 0.5),
                amp_bp(0.0042, 0.5),
                amp_bp(0.0056, 0.0),
            ],
            frequency: &[],
        };

        let output = render_pwle(&clip.into(), PwleRenderSettings::default()).unwrap();
        let durations_ms = output.control_point_durations_ms();

        assert_eq!(durations_ms.len(), output.control_points.len());
        assert_eq!(
            durations_ms.iter().sum::<u64>(),
            6,
            "total should be the rounded 5.6 ms, not the 4 ms of truncated segments"
        );
    }

    #[test]
    fn negative_emphasis_time_is_an_error() {
        let clip = TestClip {
            amplitude: &[
                amp_bp(0.0, 0.5),
                emphasis_bp(-0.5, 0.5, 1.0, 0.0),
                amp_bp(1.0, 0.5),
            ],
            frequency: &[],
        };

        let result = render_pwle(&clip.into(), PwleRenderSettings::default());
        assert!(result.is_err());
    }

    #[test]
    fn insufficient_amplitude_points() {
        let clip = TestClip {
            amplitude: &[amp_bp(0.0, 0.5)],
            frequency: &[],
        };

        let result = render_pwle(&clip.into(), PwleRenderSettings::default());
        assert!(result.is_err());
    }
}
