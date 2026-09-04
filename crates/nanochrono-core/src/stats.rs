// SPDX-License-Identifier: Apache-2.0
//! Sample statistics for timing distributions.
//!
//! Timing samples are heavily right-skewed — a scheduler preemption or a cache
//! miss adds an arbitrarily long tail — so the mean alone is misleading. These
//! summaries always carry the minimum (the closest thing to a noise-free
//! measurement) and high percentiles (where the tail lives).

/// Summary of a set of timing samples, in raw counter units.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SampleStats {
    pub count: u64,
    pub min: u64,
    pub max: u64,
    pub mean: f64,
    pub median: u64,
    pub p90: u64,
    pub p99: u64,
    pub variance: f64,
    pub stdev: f64,
}

impl SampleStats {
    /// Summarises `samples`. Returns `None` for an empty slice.
    ///
    /// Sorts a copy rather than the caller's data, so a sample buffer keeps
    /// its acquisition order for anyone who needs it.
    pub fn analyze(samples: &[u64]) -> Option<SampleStats> {
        if samples.is_empty() {
            return None;
        }
        let count = samples.len();
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();

        let sum: u128 = samples.iter().map(|&v| v as u128).sum();
        let mean = sum as f64 / count as f64;
        let variance = samples
            .iter()
            .map(|&v| {
                let d = v as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / count as f64;

        Some(SampleStats {
            count: count as u64,
            min: sorted[0],
            max: sorted[count - 1],
            mean,
            median: percentile_sorted(&sorted, 50.0),
            p90: percentile_sorted(&sorted, 90.0),
            p99: percentile_sorted(&sorted, 99.0),
            variance,
            stdev: variance.sqrt(),
        })
    }
}

/// Percentile of an already-sorted slice, by nearest-rank.
pub fn percentile_sorted(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let p = percentile.clamp(0.0, 100.0);
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Percentile of an unsorted slice.
pub fn percentile(samples: &[u64], percentile_value: f64) -> u64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    percentile_sorted(&sorted, percentile_value)
}

/// Median of an unsorted slice.
pub fn median(samples: &[u64]) -> u64 {
    percentile(samples, 50.0)
}

/// Welch's t statistic for two samples with unequal variance.
///
/// This is the standard constant-time test: run a candidate over a fixed input
/// and a random one, and if the timing distributions separate, the code
/// branched on secret data. `|t| >= 4.5` is the conventional threshold, which
/// [`ConstantTimeVerdict`] applies.
pub fn welch_t(a: &[u64], b: &[u64]) -> f64 {
    if a.len() < 2 || b.len() < 2 {
        return 0.0;
    }
    let (na, nb) = (a.len() as f64, b.len() as f64);
    let ma = a.iter().map(|&v| v as f64).sum::<f64>() / na;
    let mb = b.iter().map(|&v| v as f64).sum::<f64>() / nb;

    // Bessel-corrected sample variance: these are samples, not populations.
    let va = a.iter().map(|&v| (v as f64 - ma).powi(2)).sum::<f64>() / (na - 1.0);
    let vb = b.iter().map(|&v| (v as f64 - mb).powi(2)).sum::<f64>() / (nb - 1.0);

    let denominator = (va / na + vb / nb).sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        (ma - mb) / denominator
    }
}

/// Threshold above which a timing difference is treated as a real signal.
pub const LEAK_T_THRESHOLD: f64 = 4.5;

/// Outcome of a constant-time audit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConstantTimeVerdict {
    pub fixed: SampleStats,
    pub random: SampleStats,
    pub welch_t: f64,
    /// `|t| >= LEAK_T_THRESHOLD`.
    pub likely_leak: bool,
}

impl ConstantTimeVerdict {
    pub fn new(fixed: SampleStats, random: SampleStats) -> Self {
        ConstantTimeVerdict {
            fixed,
            random,
            welch_t: 0.0,
            likely_leak: false,
        }
    }

    /// Mean difference between the two distributions, in raw units.
    pub fn mean_delta(&self) -> f64 {
        (self.fixed.mean - self.random.mean).abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_reports_order_statistics() {
        let s = SampleStats::analyze(&[10, 20, 30, 40, 50]).unwrap();
        assert_eq!(s.count, 5);
        assert_eq!(s.min, 10);
        assert_eq!(s.max, 50);
        assert_eq!(s.median, 30);
        assert!((s.mean - 30.0).abs() < 1e-9);
    }

    #[test]
    fn analyze_rejects_empty_input() {
        assert!(SampleStats::analyze(&[]).is_none());
    }

    #[test]
    fn identical_distributions_score_zero() {
        let a: Vec<u64> = (0..100).collect();
        assert!(welch_t(&a, &a).abs() < 1e-9);
    }

    #[test]
    fn separated_distributions_exceed_threshold() {
        let a: Vec<u64> = (0..200).map(|i| 100 + i % 5).collect();
        let b: Vec<u64> = (0..200).map(|i| 400 + i % 5).collect();
        assert!(welch_t(&a, &b).abs() > LEAK_T_THRESHOLD);
    }

    #[test]
    fn analyze_leaves_caller_samples_in_order() {
        let samples = vec![50, 10, 30];
        let _ = SampleStats::analyze(&samples);
        assert_eq!(samples, vec![50, 10, 30]);
    }
}
