//! Bounded-memory summaries of all priced signals, before decision gates.

use crate::ExecutionDecision;
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct ConfidenceStats {
    pub count: u64,
    pub invalid: u64,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub mean: f64,
    /// [0, .1), [.1, .2), ..., [.9, 1] for valid probabilities.
    pub deciles: [u64; 10],
    pub executed: u64,
    pub low_confidence: u64,
    pub no_edge: u64,
    pub neutral: u64,
    pub position_limit: u64,
}

impl ConfidenceStats {
    pub fn record(&mut self, confidence: f32, decision: &ExecutionDecision) {
        self.count += 1;
        if confidence.is_finite() && (0.0..=1.0).contains(&confidence) {
            self.min = Some(self.min.map_or(confidence, |v| v.min(confidence)));
            self.max = Some(self.max.map_or(confidence, |v| v.max(confidence)));
            self.mean += (confidence as f64 - self.mean) / (self.count - self.invalid) as f64;
            self.deciles[((confidence * 10.0) as usize).min(9)] += 1;
        } else {
            self.invalid += 1;
        }
        match decision {
            ExecutionDecision::Executed { .. } => self.executed += 1,
            ExecutionDecision::RejectedLowConfidence { .. } => self.low_confidence += 1,
            ExecutionDecision::RejectedNoEdge { .. } => self.no_edge += 1,
            ExecutionDecision::Neutral { .. } => self.neutral += 1,
            ExecutionDecision::RejectedPositionLimit { .. } => self.position_limit += 1,
        }
    }

    pub fn log(&self) {
        tracing::info!(
            "Confidence summary: {}",
            serde_json::to_string(self).expect("finite stats")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_includes_rejected_signals_and_endpoints() {
        let mut stats = ConfidenceStats::default();
        for confidence in [0.0, 0.15, 0.25, 1.0, f32::NAN, f32::INFINITY, -0.1] {
            stats.record(
                confidence,
                &ExecutionDecision::RejectedLowConfidence {
                    reason: String::new(),
                    latency_ns: 0,
                },
            );
        }
        assert_eq!(stats.count, 7);
        assert_eq!(stats.invalid, 3);
        assert_eq!(stats.low_confidence, 7);
        assert_eq!(stats.deciles, [1, 1, 1, 0, 0, 0, 0, 0, 0, 1]);
        assert!((stats.mean - 0.35).abs() < 1e-7);
        assert_eq!(stats.min, Some(0.0));
        assert_eq!(stats.max, Some(1.0));
    }
}
