//! A minute of real, timestamped observations, independent of the polling rate.

use std::collections::VecDeque;

use crate::metrics::Snapshot;

const WINDOW_SECONDS: f64 = 60.0;
// A 0.5-second polling interval needs 121 observations, plus one observation
// immediately before the window when refresh intervals change.
const MAX_SAMPLES: usize = 122;

#[derive(Default)]
pub struct History {
    samples: VecDeque<(f64, Snapshot)>,
}

impl History {
    /// Store a monotonic observation. Invalid or out-of-order timestamps are
    /// ignored; a repeated timestamp replaces the observation at that instant.
    pub fn push(&mut self, at: f64, snapshot: Snapshot) {
        if !at.is_finite() {
            return;
        }
        if let Some((last_at, _)) = self.samples.back() {
            if at < *last_at {
                return;
            }
            if at == *last_at {
                self.samples.pop_back();
            }
        }
        self.samples.push_back((at, snapshot));

        let cutoff = at - WINDOW_SECONDS;
        // Preserve the observation straddling the boundary so the chart can
        // clip the segment precisely rather than changing its apparent slope.
        while self.samples.len() > 1 && self.samples[1].0 <= cutoff {
            self.samples.pop_front();
        }
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    pub fn latest(&self) -> Option<&Snapshot> {
        self.samples.back().map(|(_, snapshot)| snapshot)
    }

    pub fn latest_time(&self) -> f64 {
        self.samples.back().map_or(0.0, |(at, _)| *at)
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// The amount of measured history available, capped to the visible minute.
    pub fn span_seconds(&self) -> f64 {
        self.samples.front().map_or(0.0, |(at, _)| {
            (self.latest_time() - at).clamp(0.0, WINDOW_SECONDS)
        })
    }

    /// Select one metric as chart points relative to the latest observation.
    /// Missing and non-finite values become NaN to break the chart's line;
    /// interpolation never crosses such a gap. The first minute is not filled
    /// with invented observations.
    pub fn series(&self, select: impl Fn(&Snapshot) -> Option<f64>) -> Vec<[f64; 2]> {
        let latest = self.latest_time();
        let cutoff = latest - WINDOW_SECONDS;
        let value = |snapshot: &Snapshot| {
            select(snapshot)
                .filter(|number| number.is_finite())
                .unwrap_or(f64::NAN)
        };
        let mut points = Vec::with_capacity(self.samples.len());
        for (index, (at, snapshot)) in self.samples.iter().enumerate() {
            let y = value(snapshot);
            if *at < cutoff {
                if let Some((next_at, next_snapshot)) = self.samples.get(index + 1)
                    && *next_at > cutoff
                {
                    let next_y = value(next_snapshot);
                    let fraction = (cutoff - at) / (next_at - at);
                    let boundary_y = if y.is_finite() && next_y.is_finite() {
                        y * (1.0 - fraction) + next_y * fraction
                    } else {
                        f64::NAN
                    };
                    points.push([-WINDOW_SECONDS, boundary_y]);
                }
            } else {
                points.push([at - latest, y]);
            }
        }
        points
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(cpu: Option<f64>) -> Snapshot {
        Snapshot {
            cpu_percent: cpu,
            ..Snapshot::default()
        }
    }

    #[test]
    fn startup_uses_only_measured_history() {
        let mut history = History::default();
        assert!(history.latest().is_none());
        assert!(history.series(|s| s.cpu_percent).is_empty());
        history.push(20.0, observation(Some(10.0)));
        history.push(25.0, observation(Some(30.0)));
        assert_eq!(history.span_seconds(), 5.0);
        assert_eq!(
            history.series(|s| s.cpu_percent),
            vec![[-5.0, 10.0], [0.0, 30.0]]
        );
    }

    #[test]
    fn changed_polling_interval_keeps_real_timestamps() {
        let mut history = History::default();
        for at in [0.0, 5.0, 10.0, 10.5, 11.0, 16.0] {
            history.push(at, observation(Some(at)));
        }
        let times: Vec<_> = history
            .series(|s| s.cpu_percent)
            .into_iter()
            .map(|p| p[0])
            .collect();
        assert_eq!(times, [-16.0, -11.0, -6.0, -5.5, -5.0, 0.0]);
    }

    #[test]
    fn boundary_is_clipped_with_linear_interpolation() {
        let mut history = History::default();
        for at in [0.0, 5.0, 10.0, 62.0] {
            history.push(at, observation(Some(at * 2.0)));
        }
        let points = history.series(|s| s.cpu_percent);
        assert_eq!(
            points,
            vec![[-60.0, 4.0], [-57.0, 10.0], [-52.0, 20.0], [0.0, 124.0]]
        );
        assert_eq!(history.span_seconds(), 60.0);
        history.push(65.0, observation(Some(130.0)));
        assert_eq!(history.series(|s| s.cpu_percent)[0], [-60.0, 10.0]);
        assert_eq!(history.len(), 4);
    }

    #[test]
    fn missing_or_invalid_metrics_preserve_gaps_including_at_boundary() {
        let mut history = History::default();
        for (at, cpu) in [
            (0.0, None),
            (5.0, Some(5.0)),
            (10.0, Some(f64::INFINITY)),
            (62.0, Some(62.0)),
        ] {
            history.push(at, observation(cpu));
        }
        let points = history.series(|s| s.cpu_percent);
        assert_eq!(points[0][0], -60.0);
        assert!(points[0][1].is_nan());
        assert_eq!(points[1], [-57.0, 5.0]);
        assert!(points[2][1].is_nan());
        assert_eq!(points[3], [0.0, 62.0]);
    }

    #[test]
    fn invalid_timestamps_do_not_corrupt_history() {
        let mut history = History::default();
        history.push(10.0, observation(Some(1.0)));
        history.push(f64::NAN, observation(Some(2.0)));
        history.push(f64::INFINITY, observation(Some(3.0)));
        history.push(9.0, observation(Some(4.0)));
        history.push(10.0, observation(Some(5.0)));
        assert_eq!(history.len(), 1);
        assert_eq!(history.latest_time(), 10.0);
        assert_eq!(history.latest().unwrap().cpu_percent, Some(5.0));
    }

    #[test]
    fn fastest_refresh_retains_a_minute_with_bounded_storage() {
        let mut history = History::default();
        for step in 0..10_000 {
            history.push(f64::from(step) * 0.5, observation(Some(50.0)));
        }
        assert_eq!(history.len(), 121);
        assert_eq!(history.span_seconds(), 60.0);
        let points = history.series(|s| s.cpu_percent);
        assert_eq!(points.first().unwrap()[0], -60.0);
        assert_eq!(points.last().unwrap()[0], 0.0);
    }
}
