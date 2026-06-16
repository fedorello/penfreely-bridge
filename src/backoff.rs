//! Exponential reconnect backoff with a ceiling.

use std::time::Duration;

/// Tracks the delay between reconnect attempts: it doubles after each failure up
/// to a maximum, and resets to the initial delay after a successful connection.
#[derive(Debug, Clone)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl Backoff {
    /// Create a backoff starting at `initial`, capped at `max`.
    #[must_use]
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            current: initial,
        }
    }

    /// The delay to wait before the next attempt, then double it (up to `max`).
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    /// Reset to the initial delay after a successful connection.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_each_attempt_up_to_the_ceiling() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(8));

        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
        // Capped at the ceiling.
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
    }

    #[test]
    fn reset_returns_to_the_initial_delay() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(8));
        backoff.next_delay();
        backoff.next_delay();

        backoff.reset();

        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }
}
