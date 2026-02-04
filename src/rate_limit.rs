//! Rate limiting module for the Janus application.
//!
//! Provides configuration for rate limiting API endpoints to prevent abuse,
//! particularly for token generation endpoints. The actual rate limiting
//! implementation uses tower_governor and is applied in the main server setup.

/// Configuration for rate limiting
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RateLimitConfig {
    /// Maximum number of requests allowed per period
    pub requests_per_period: u32,
    /// Time period in seconds for the rate limit
    pub period_secs: u64,
    /// Trust X-Forwarded-For header for IP extraction (enable for reverse proxies)
    #[serde(default)]
    pub trust_proxy: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            requests_per_period: 3,  // 3 requests
            period_secs: 60,          // per minute
            trust_proxy: false,       // Don't trust X-Forwarded-For by default
        }
    }
}

impl RateLimitConfig {
    /// Create a new rate limit configuration
    pub fn new(requests_per_period: u32, period_secs: u64) -> Self {
        RateLimitConfig {
            requests_per_period,
            period_secs,
            trust_proxy: false,
        }
    }

    /// Create a new rate limit configuration with proxy trust setting
    pub fn with_proxy_trust(requests_per_period: u32, period_secs: u64, trust_proxy: bool) -> Self {
        RateLimitConfig {
            requests_per_period,
            period_secs,
            trust_proxy,
        }
    }

    /// Get the replenishment period in seconds
    ///
    /// This is the time it takes for one request token to be replenished.
    /// For example, if you want 3 requests per 60 seconds, the replenishment
    /// period is 60/3 = 20 seconds.
    pub fn replenish_interval_secs(&self) -> u64 {
        self.period_secs / (self.requests_per_period as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.requests_per_period, 3);
        assert_eq!(config.period_secs, 60);
        assert_eq!(config.replenish_interval_secs(), 20);
    }

    #[test]
    fn test_custom_config() {
        let config = RateLimitConfig::new(10, 120);
        assert_eq!(config.requests_per_period, 10);
        assert_eq!(config.period_secs, 120);
        assert_eq!(config.replenish_interval_secs(), 12);
    }

    #[test]
    fn test_config_clone() {
        let config = RateLimitConfig::new(5, 30);
        let cloned = config.clone();
        assert_eq!(config.requests_per_period, cloned.requests_per_period);
        assert_eq!(config.period_secs, cloned.period_secs);
    }

    #[test]
    fn test_replenish_interval() {
        let config = RateLimitConfig::new(6, 60);
        assert_eq!(config.replenish_interval_secs(), 10);

        let config = RateLimitConfig::new(1, 60);
        assert_eq!(config.replenish_interval_secs(), 60);
    }
}
