use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CONCURRENT_REQUESTS: usize = 6;

pub struct Ratelimiter {
    // Store timestamp as u64 (milliseconds since epoch)
    ratelimited_until: Arc<AtomicU64>,
    semaphore: Arc<Semaphore>,
}

impl Ratelimiter {
    pub fn new() -> Self {
        Self {
            ratelimited_until: Arc::new(AtomicU64::new(0)), // 0 means no rate limiting
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        }
    }

    pub async fn wait(&self) -> OwnedSemaphorePermit {
        let permit = self.semaphore.clone().acquire_owned().await.unwrap();

        let until_timestamp = self.ratelimited_until.load(Ordering::Acquire);
        if until_timestamp > 0 {
            let now = instant_to_u64(Instant::now());
            if now < until_timestamp {
                tokio::time::sleep(Duration::from_millis(until_timestamp - now)).await;
            } else {
                // If the ratelimited_until is in the past, reset it to 0 so we dont check next time
                self.ratelimited_until.store(0, Ordering::Release);
            }
        }

        permit
    }

    pub fn set_ratelimited_for(&self, for_seconds: u64) {
        let until = Instant::now() + Duration::from_secs(for_seconds);
        self.ratelimited_until
            .store(instant_to_u64(until), Ordering::Release);
    }
}

// Convert Instant to milliseconds since some base point
fn instant_to_u64(instant: Instant) -> u64 {
    // We convert to duration since UNIX_EPOCH, falling back to duration since process start if that fails
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // For a future instant, we add the duration from now to that instant
    // For a past instant, we subtract the elapsed duration
    let now_instant = Instant::now();
    if instant > now_instant {
        // Future time: add the difference
        let duration_until = instant.duration_since(now_instant);
        now + duration_until.as_millis() as u64
    } else {
        // Past time: subtract the difference
        let elapsed = now_instant.duration_since(instant);
        now - elapsed.as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn test_new_ratelimiter() {
        let ratelimiter = Ratelimiter::new();
        assert_eq!(ratelimiter.ratelimited_until.load(Ordering::Acquire), 0);
        assert_eq!(
            ratelimiter.semaphore.available_permits(),
            MAX_CONCURRENT_REQUESTS
        );
    }

    #[tokio::test]
    async fn test_wait_no_rate_limit() {
        let ratelimiter = Ratelimiter::new();
        let start = Instant::now();
        let _permit = ratelimiter.wait().await;
        let elapsed = start.elapsed();

        // Should return immediately when no rate limiting
        assert!(elapsed.as_millis() < 100);
    }

    #[tokio::test]
    async fn test_semaphore_limit() {
        let ratelimiter = Ratelimiter::new();
        let mut permits = Vec::new();

        // Acquire all available permits
        for _ in 0..MAX_CONCURRENT_REQUESTS {
            permits.push(ratelimiter.wait().await);
        }

        // Next permit should block, so use timeout to check
        let next_permit_future = ratelimiter.wait();
        let timeout_result = timeout(Duration::from_millis(100), next_permit_future).await;

        // Should timeout as all permits are taken
        assert!(timeout_result.is_err());

        // Drop one permit
        permits.pop();

        // Now we should be able to acquire a permit
        let _new_permit = ratelimiter.wait().await;
    }

    #[tokio::test]
    async fn test_rate_limited() {
        let ratelimiter = Ratelimiter::new();

        // Set rate limit for 1 second
        ratelimiter.set_ratelimited_for(1);

        let start = Instant::now();
        let _permit = ratelimiter.wait().await;
        let elapsed = start.elapsed();

        // Should have waited for the rate limit
        assert!(elapsed.as_secs() >= 1);
    }

    #[tokio::test]
    async fn test_expired_rate_limit() {
        let ratelimiter = Ratelimiter::new();

        // Set a very short rate limit
        let until = Instant::now() + Duration::from_millis(10);
        ratelimiter
            .ratelimited_until
            .store(instant_to_u64(until), Ordering::Release);

        // Wait longer than the rate limit
        sleep(Duration::from_millis(20)).await;

        let start = Instant::now();
        let _permit = ratelimiter.wait().await;
        let elapsed = start.elapsed();

        // Should return immediately as rate limit has expired
        assert!(elapsed.as_millis() < 50);

        // Rate limit should be reset to 0
        assert_eq!(ratelimiter.ratelimited_until.load(Ordering::Acquire), 0);
    }
}
