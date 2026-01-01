//! Simplified circuit breaker for arbitrage trading.
//!
//! Since arbitrage is risk-free on matched fills, this module only tracks:
//! - Total position (capital tied up in unresolved markets)
//! - Consecutive errors (halt if something is broken)

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{error, warn, info};

/// Circuit breaker configuration from environment
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Maximum total position across all markets (in contracts)
    pub max_total_position: i64,

    /// Maximum number of consecutive errors before halting
    pub max_consecutive_errors: u32,

    /// Cooldown period after a trip (seconds)
    pub cooldown_secs: u64,

    /// Whether circuit breakers are enabled
    pub enabled: bool,
}

impl CircuitBreakerConfig {
    pub fn from_env() -> Self {
        Self {
            max_total_position: std::env::var("CB_MAX_TOTAL_POSITION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100000),

            max_consecutive_errors: std::env::var("CB_MAX_CONSECUTIVE_ERRORS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),

            cooldown_secs: std::env::var("CB_COOLDOWN_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300), // 5 minutes default

            enabled: std::env::var("CB_ENABLED")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(true), // Enabled by default for safety
        }
    }
}

/// Reason why circuit breaker was tripped
#[derive(Debug, Clone, PartialEq)]
pub enum TripReason {
    MaxTotalPosition { position: i64, limit: i64 },
    ConsecutiveErrors { count: u32, limit: u32 },
    ManualHalt,
}

impl std::fmt::Display for TripReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TripReason::MaxTotalPosition { position, limit } => {
                write!(f, "Max total position: {} contracts (limit: {})", position, limit)
            }
            TripReason::ConsecutiveErrors { count, limit } => {
                write!(f, "Consecutive errors: {} (limit: {})", count, limit)
            }
            TripReason::ManualHalt => {
                write!(f, "Manual halt triggered")
            }
        }
    }
}

/// Circuit breaker state
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,

    /// Whether trading is currently halted
    halted: AtomicBool,

    /// When the circuit breaker was tripped
    tripped_at: RwLock<Option<Instant>>,

    /// Reason for trip
    trip_reason: RwLock<Option<TripReason>>,

    /// Consecutive error count
    consecutive_errors: AtomicI64,

    /// Total position across all markets (in contracts)
    total_position: AtomicI64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        info!("[CB] Circuit breaker initialized:");
        info!("[CB]   Enabled: {}", config.enabled);
        info!("[CB]   Max total position: {} contracts", config.max_total_position);
        info!("[CB]   Max consecutive errors: {}", config.max_consecutive_errors);
        info!("[CB]   Cooldown: {}s", config.cooldown_secs);

        Self {
            config,
            halted: AtomicBool::new(false),
            tripped_at: RwLock::new(None),
            trip_reason: RwLock::new(None),
            consecutive_errors: AtomicI64::new(0),
            total_position: AtomicI64::new(0),
        }
    }

    /// Check if trading is allowed
    #[allow(dead_code)]
    pub fn is_trading_allowed(&self) -> bool {
        if !self.config.enabled {
            return true;
        }
        !self.halted.load(Ordering::SeqCst)
    }

    /// Check if we can execute a trade
    pub async fn can_execute(&self, _market_id: &str, contracts: i64) -> Result<(), TripReason> {
        if !self.config.enabled {
            return Ok(());
        }

        if self.halted.load(Ordering::SeqCst) {
            let reason = self.trip_reason.read().await;
            return Err(reason.clone().unwrap_or(TripReason::ManualHalt));
        }

        // Total position limit
        let current = self.total_position.load(Ordering::SeqCst);
        if current + contracts > self.config.max_total_position {
            return Err(TripReason::MaxTotalPosition {
                position: current + contracts,
                limit: self.config.max_total_position,
            });
        }

        Ok(())
    }

    /// Record a successful execution
    pub async fn record_success(&self, _market_id: &str, matched_contracts: i64) {
        // Reset consecutive errors
        self.consecutive_errors.store(0, Ordering::SeqCst);

        // Update total position (both YES and NO count as position)
        self.total_position.fetch_add(matched_contracts * 2, Ordering::SeqCst);
    }

    /// Record an error
    pub async fn record_error(&self) {
        let errors = self.consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;

        if errors >= self.config.max_consecutive_errors as i64 {
            self.trip(TripReason::ConsecutiveErrors {
                count: errors as u32,
                limit: self.config.max_consecutive_errors,
            }).await;
        }
    }

    /// Trip the circuit breaker
    pub async fn trip(&self, reason: TripReason) {
        if !self.config.enabled {
            return;
        }

        error!("🚨 CIRCUIT BREAKER TRIPPED: {}", reason);

        self.halted.store(true, Ordering::SeqCst);
        *self.tripped_at.write().await = Some(Instant::now());
        *self.trip_reason.write().await = Some(reason);
    }

    /// Manually halt trading
    #[allow(dead_code)]
    pub async fn halt(&self) {
        warn!("[CB] Manual halt triggered");
        self.trip(TripReason::ManualHalt).await;
    }

    /// Reset the circuit breaker (after cooldown or manual reset)
    #[allow(dead_code)]
    pub async fn reset(&self) {
        info!("[CB] Circuit breaker reset");
        self.halted.store(false, Ordering::SeqCst);
        *self.tripped_at.write().await = None;
        *self.trip_reason.write().await = None;
        self.consecutive_errors.store(0, Ordering::SeqCst);
    }

    /// Check if cooldown has elapsed and auto-reset if so
    #[allow(dead_code)]
    pub async fn check_cooldown(&self) -> bool {
        if !self.halted.load(Ordering::SeqCst) {
            return true;
        }

        let tripped_at = self.tripped_at.read().await;
        if let Some(tripped) = *tripped_at {
            if tripped.elapsed() > Duration::from_secs(self.config.cooldown_secs) {
                drop(tripped_at); // Release read lock before reset
                self.reset().await;
                return true;
            }
        }

        false
    }

    /// Get current status
    #[allow(dead_code)]
    pub async fn status(&self) -> CircuitBreakerStatus {
        CircuitBreakerStatus {
            enabled: self.config.enabled,
            halted: self.halted.load(Ordering::SeqCst),
            trip_reason: self.trip_reason.read().await.clone(),
            consecutive_errors: self.consecutive_errors.load(Ordering::SeqCst) as u32,
            total_position: self.total_position.load(Ordering::SeqCst),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CircuitBreakerStatus {
    pub enabled: bool,
    pub halted: bool,
    pub trip_reason: Option<TripReason>,
    pub consecutive_errors: u32,
    pub total_position: i64,
}

impl std::fmt::Display for CircuitBreakerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.enabled {
            return write!(f, "Circuit Breaker: DISABLED");
        }

        if self.halted {
            write!(f, "Circuit Breaker: 🛑 HALTED")?;
            if let Some(reason) = &self.trip_reason {
                write!(f, " ({})", reason)?;
            }
        } else {
            write!(f, "Circuit Breaker: ✅ OK")?;
        }

        write!(f, " | Pos: {} contracts | Errors: {}",
               self.total_position, self.consecutive_errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker_total_position_limit() {
        let config = CircuitBreakerConfig {
            max_total_position: 50,
            max_consecutive_errors: 3,
            cooldown_secs: 60,
            enabled: true,
        };

        let cb = CircuitBreaker::new(config);

        // Should allow initial trade
        assert!(cb.can_execute("market1", 20).await.is_ok());

        // Record the trade (20 matched = 40 total position for YES+NO)
        cb.record_success("market1", 20).await;

        // Should reject trade exceeding total limit (40 + 20 > 50)
        let result = cb.can_execute("market2", 20).await;
        assert!(matches!(result, Err(TripReason::MaxTotalPosition { .. })));
    }

    #[tokio::test]
    async fn test_consecutive_errors() {
        let config = CircuitBreakerConfig {
            max_total_position: 500,
            max_consecutive_errors: 3,
            cooldown_secs: 60,
            enabled: true,
        };

        let cb = CircuitBreaker::new(config);

        // Record errors
        cb.record_error().await;
        cb.record_error().await;
        assert!(cb.is_trading_allowed());

        // Third error should trip
        cb.record_error().await;
        assert!(!cb.is_trading_allowed());
    }
}
