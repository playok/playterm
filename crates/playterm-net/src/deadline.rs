//! The time limit for connecting (Tools → Settings): from the TCP connect until the session is
//! ready. Time spent waiting on the user — a host key or certificate to confirm, a password to
//! type — does not count, so only a server that stops answering runs into it.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use playterm_i18n::tr;
use tokio::time::Instant;

/// How long a paused limit waits before looking again.
const PAUSED_POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct State {
    /// When the limit runs out; `None` outside [`Deadline::limit`].
    until: Option<Instant>,
    /// Since when the user is being waited on (see [`Deadline::pause`]).
    paused_since: Option<Instant>,
    /// Nested pauses.
    pauses: usize,
}

/// Shared by a session's task and its [`crate::session::EventSink`], whose prompts pause it.
#[derive(Clone, Default)]
pub(crate) struct Deadline(Arc<Mutex<State>>);

impl Deadline {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `connecting`, failing with "connection timed out" if it takes longer than `limit`
    /// (not counting pauses).
    pub(crate) async fn limit<T>(
        &self,
        limit: Duration,
        connecting: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        {
            let mut state = self.state();
            state.until = Some(Instant::now() + limit);
            state.paused_since = None;
            state.pauses = 0;
        }
        let result = tokio::select! {
            result = connecting => result,
            () = self.passed() => Err(anyhow::anyhow!(tr!(
                "연결 시간 초과 ({}초 동안 응답 없음). 시간 제한은 도구 → 설정에서 바꿀 수 있습니다.",
                "Connection timed out (no answer for {} s). The limit can be changed in Tools → Settings.",
                limit.as_secs()
            ))),
        };
        self.state().until = None;
        result
    }

    /// Stops the clock until the returned guard is dropped: the user is being waited on.
    pub(crate) fn pause(&self) -> Pause<'_> {
        let mut state = self.state();
        if state.pauses == 0 {
            state.paused_since = Some(Instant::now());
        }
        state.pauses += 1;
        Pause(self)
    }

    /// Resolves once the limit has run out.
    async fn passed(&self) {
        loop {
            let (until, paused) = {
                let state = self.state();
                (state.until, state.paused_since.is_some())
            };
            let wake = match until {
                None => return std::future::pending().await,
                Some(_) if paused => Instant::now() + PAUSED_POLL,
                Some(until) if Instant::now() >= until => return,
                // A pause may move it later meanwhile; it is looked at again on waking.
                Some(until) => until,
            };
            tokio::time::sleep_until(wake).await;
        }
    }
}

/// See [`Deadline::pause`].
pub(crate) struct Pause<'a>(&'a Deadline);

impl Drop for Pause<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.pauses = state.pauses.saturating_sub(1);
        if state.pauses == 0
            && let Some(since) = state.paused_since.take()
            && let Some(until) = state.until.as_mut()
        {
            *until += since.elapsed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn times_out() {
        let deadline = Deadline::default();
        let started = Instant::now();
        let result = deadline.limit(Duration::from_secs(5), std::future::pending::<anyhow::Result<()>>()).await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains('5'), "{error}");
        assert_eq!(started.elapsed().as_secs(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn finishes_in_time() {
        let deadline = Deadline::default();
        let result = deadline
            .limit(Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_secs(4)).await;
                Ok(7)
            })
            .await;
        assert_eq!(result.unwrap(), 7);
        // Outside `limit` nothing runs out.
        assert!(deadline.state().until.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_on_the_user_does_not_count() {
        let deadline = Deadline::default();
        let started = Instant::now();
        let result = deadline
            .limit(Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                {
                    // The user takes a minute over a prompt.
                    let _pause = deadline.pause();
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            })
            .await;
        assert!(result.is_ok(), "3 s + 1 s of network time is within 5 s");
        assert_eq!(started.elapsed().as_secs(), 64);

        // After a pause the rest of the time still runs out.
        let started = Instant::now();
        let result = deadline
            .limit(Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                drop(deadline.pause());
                std::future::pending::<anyhow::Result<()>>().await
            })
            .await;
        assert!(result.is_err());
        assert_eq!(started.elapsed().as_secs(), 5);
    }
}
