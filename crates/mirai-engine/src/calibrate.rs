// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Explicit, measurement-based tuning for a managed local KataGo profile.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mirai_core::{Color, RuleSet, Size};
use mirai_proto::types::{AnalyzeReq, Want};

use crate::{Engine, EngineError, EngineTuning, LocalEngine, LocalEngineConfig};

const SEARCH_CANDIDATES: [u16; 6] = [2, 4, 8, 16, 32, 64];
const ANALYSIS_CANDIDATES: [u16; 3] = [1, 2, 4];
const TOTAL_CANDIDATES: u8 = 8;
const WARMUP_VISITS: u32 = 32;
const MEASURE_VISITS: u32 = 512;
const MIN_ADJACENT_GAIN: f64 = 0.10;

#[derive(Clone, Debug)]
pub struct CalibrationConfig {
    pub name: String,
    pub katago: PathBuf,
    pub model: PathBuf,
    pub log_dir: PathBuf,
    pub tuning: EngineTuning,
    pub startup_timeout: Duration,
}

impl CalibrationConfig {
    pub fn new(
        name: impl Into<String>,
        katago: impl Into<PathBuf>,
        model: impl Into<PathBuf>,
        log_dir: impl Into<PathBuf>,
        tuning: EngineTuning,
    ) -> CalibrationConfig {
        CalibrationConfig {
            name: name.into(),
            katago: katago.into(),
            model: model.into(),
            log_dir: log_dir.into(),
            tuning,
            startup_timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationProgress {
    pub completed: u8,
    pub total: u8,
    pub analysis_threads: u16,
    pub search_threads: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibrationSample {
    pub at: u16,
    pub st: u16,
    pub visits: u64,
    pub elapsed: Duration,
}
impl CalibrationSample {
    pub fn visits_per_second(&self) -> f64 {
        self.visits as f64 / self.elapsed.as_secs_f64()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationResult {
    pub tuning: EngineTuning,
    pub samples: Vec<CalibrationSample>,
}

/// Runs eight isolated candidates. Dropping this future drops its live subscription and
/// engine; there is deliberately no second query-cancellation path.
pub async fn calibrate(
    config: CalibrationConfig,
    progress_callback: impl Fn(CalibrationProgress) + Send + 'static,
) -> Result<CalibrationResult, EngineError> {
    let mut samples = Vec::with_capacity(usize::from(TOTAL_CANDIDATES));
    for (index, search_threads) in SEARCH_CANDIDATES.into_iter().enumerate() {
        progress_callback(CalibrationProgress {
            completed: index as u8,
            total: TOTAL_CANDIDATES,
            analysis_threads: 1,
            search_threads,
        });
        samples.push(measure_candidate(&config, 1, search_threads).await?);
    }
    let search_rates: Vec<(u16, f64)> = samples
        .iter()
        .map(|s| (s.st, s.visits_per_second()))
        .collect();
    let search_threads = select_search_threads(&search_rates);
    for (offset, analysis_threads) in ANALYSIS_CANDIDATES[1..].iter().copied().enumerate() {
        progress_callback(CalibrationProgress {
            completed: (SEARCH_CANDIDATES.len() + offset) as u8,
            total: TOTAL_CANDIDATES,
            analysis_threads,
            search_threads,
        });
        samples.push(measure_candidate(&config, analysis_threads, search_threads).await?);
    }
    let analysis_rates: Vec<(u16, f64)> = samples
        .iter()
        .filter(|s| s.st == search_threads)
        .map(|s| (s.at, s.visits_per_second()))
        .collect();
    let analysis_threads = select_analysis_threads(&analysis_rates);
    Ok(CalibrationResult {
        tuning: final_tuning(config.tuning, analysis_threads, search_threads),
        samples,
    })
}

async fn measure_candidate(
    config: &CalibrationConfig,
    at: u16,
    st: u16,
) -> Result<CalibrationSample, EngineError> {
    let tuning = final_tuning(config.tuning, at, st);
    let config_path = tuning
        .write_to(&config.log_dir)
        .map_err(|e| EngineError::Startup(format!("could not write calibration config: {e}")))?;
    let mut local = LocalEngineConfig::new(
        config.name.clone(),
        config.katago.clone(),
        config.model.clone(),
        config_path,
    );
    local.log_dir = config.log_dir.clone();
    local.startup_timeout = config.startup_timeout;
    let engine = LocalEngine::spawn(local).await?;
    let result = measure_running_engine(&engine, at, st).await;
    engine.shutdown().await;
    result
}

async fn measure_running_engine(
    engine: &LocalEngine,
    at: u16,
    st: u16,
) -> Result<CalibrationSample, EngineError> {
    engine
        .subscribe(calibration_request(4, WARMUP_VISITS))
        .finish()
        .await?;
    let started = Instant::now();
    let subscriptions: Vec<_> = (0..at)
        .map(|position| engine.subscribe(calibration_request(position, MEASURE_VISITS)))
        .collect();
    let mut visits = 0u64;
    for subscription in subscriptions {
        visits += u64::from(subscription.finish().await?.root.visits);
    }
    Ok(CalibrationSample {
        at,
        st,
        visits,
        elapsed: started.elapsed(),
    })
}

fn calibration_request(position: u16, visits: u32) -> AnalyzeReq {
    let size = Size::square(19);
    let openings = [
        [(3, 3), (15, 15), (15, 3), (3, 15), (9, 9), (9, 3)],
        [(15, 3), (3, 15), (3, 3), (15, 15), (9, 9), (9, 15)],
        [(3, 15), (15, 3), (15, 15), (3, 3), (9, 9), (3, 9)],
        [(15, 15), (3, 3), (3, 15), (15, 3), (9, 9), (15, 9)],
        [(9, 9), (3, 3), (15, 15), (3, 15), (15, 3), (9, 15)],
    ];
    let mut request = AnalyzeReq::new(size, RuleSet::Chinese, 7.5);
    request.moves = openings[usize::from(position) % openings.len()]
        .into_iter()
        .enumerate()
        .map(|(turn, (x, y))| {
            (
                if turn % 2 == 0 {
                    Color::Black
                } else {
                    Color::White
                },
                size.point(x, y),
            )
        })
        .collect();
    request.max_visits = Some(visits);
    request.want = Want::OWNERSHIP;
    // Match mirai's live-analysis request rather than KataGo's deliberately noisy
    // analysis-engine root default.
    request
        .overrides
        .push(("wideRootNoise".into(), "0.0".into()));
    request
}

fn select_search_threads(rates: &[(u16, f64)]) -> u16 {
    assert!(!rates.is_empty(), "search calibration needs a sample");
    for pair in rates.windows(2) {
        if (pair[1].1 - pair[0].1) / pair[0].1 < MIN_ADJACENT_GAIN {
            return pair[0].0;
        }
    }
    rates[rates.len() - 1].0
}

fn select_analysis_threads(rates: &[(u16, f64)]) -> u16 {
    assert!(!rates.is_empty(), "analysis calibration needs a sample");
    rates
        .iter()
        .copied()
        .reduce(|best, candidate| {
            if candidate.1 > best.1 || (candidate.1 == best.1 && candidate.0 < best.0) {
                candidate
            } else {
                best
            }
        })
        .expect("nonempty")
        .0
}

fn final_tuning(original: EngineTuning, at: u16, st: u16) -> EngineTuning {
    EngineTuning {
        analysis_threads: at,
        search_threads: st,
        nn_max_batch_size: original.nn_max_batch_size.max(at.saturating_mul(st)),
        nn_cache_size_power_of_two: original.nn_cache_size_power_of_two,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_stops_before_low_gain() {
        assert_eq!(
            select_search_threads(&[(2, 100.0), (4, 120.0), (8, 130.0), (16, 200.0)]),
            4
        );
    }
    #[test]
    fn search_uses_last_when_all_gains_high() {
        assert_eq!(
            select_search_threads(&[(2, 100.0), (4, 111.0), (8, 124.0), (16, 140.0)]),
            16
        );
    }
    #[test]
    fn analysis_uses_maximum_throughput() {
        assert_eq!(
            select_analysis_threads(&[(1, 100.0), (2, 170.0), (4, 210.0)]),
            4
        );
    }
    #[test]
    fn analysis_tie_uses_smaller_at() {
        assert_eq!(
            select_analysis_threads(&[(4, 210.0), (2, 210.0), (1, 180.0)]),
            2
        );
    }
    #[test]
    fn benchmark_requests_are_fixed_visit_owned_and_position_distinct() {
        let first = calibration_request(0, MEASURE_VISITS);
        let second = calibration_request(1, MEASURE_VISITS);
        assert_eq!(first.max_visits, Some(MEASURE_VISITS));
        assert_eq!(first.max_time_ms, None);
        assert!(first.want.contains(Want::OWNERSHIP));
        assert_eq!(
            first.overrides,
            [("wideRootNoise".to_string(), "0.0".to_string())]
        );
        assert_ne!(
            first.moves, second.moves,
            "concurrent samples must not benchmark the same cached position"
        );
    }
    #[test]
    fn final_tuning_preserves_cache_and_covers_threads() {
        let original = EngineTuning {
            analysis_threads: 1,
            search_threads: 2,
            nn_max_batch_size: 16,
            nn_cache_size_power_of_two: 23,
        };
        let tuned = final_tuning(original, 4, 16);
        assert_eq!(
            (
                tuned.analysis_threads,
                tuned.search_threads,
                tuned.nn_max_batch_size,
                tuned.nn_cache_size_power_of_two
            ),
            (4, 16, 64, 23)
        );
        assert_eq!(
            final_tuning(
                EngineTuning {
                    nn_max_batch_size: 128,
                    ..original
                },
                2,
                8
            )
            .nn_max_batch_size,
            128
        );
    }
}
