//! A failed signal listener is unavailable, rather than an intentional shutdown request.
use crate::diagnostics::Diagnostics;
use std::{future::Future, io};
use tokio::sync::watch;

pub async fn wait<S, T>(
    signal: S,
    mut requested: watch::Receiver<bool>,
    timer: T,
    diagnostics: Diagnostics,
) where
    S: Future<Output = io::Result<()>>,
    T: Future<Output = ()>,
{
    tokio::pin!(signal, timer);
    let mut signal_available = true;
    loop {
        if *requested.borrow() {
            return;
        }
        tokio::select! {
            result = &mut signal, if signal_available => {
                match result {
                    Ok(()) => return,
                    Err(error) => {
                        diagnostics.signal_unavailable(error.raw_os_error());
                        // Do not poll the completed error future again or create a retry loop.
                        signal_available = false;
                    }
                }
            }
            _ = &mut timer => return,
            changed = requested.changed() => {
                if changed.is_err() || *requested.borrow() { return; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[tokio::test(start_paused = true)]
    async fn failed_signal_is_recorded_once_and_api_shutdown_still_works() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::new(&directory.path().join("signal.sqlite3"));
        let polls = Arc::new(AtomicUsize::new(0));
        let counted = polls.clone();
        let signal = std::future::poll_fn(move |_| {
            assert_eq!(
                counted.fetch_add(1, Ordering::SeqCst),
                0,
                "completed signal was polled again"
            );
            std::task::Poll::Ready(Err(io::Error::from_raw_os_error(6)))
        });
        let (request, receive) = watch::channel(false);
        let shutdown = tokio::spawn(wait(
            signal,
            receive,
            std::future::pending(),
            diagnostics.clone(),
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(!shutdown.is_finished());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(diagnostics.snapshot().latest.unwrap().sequence, 1);
        request.send(true).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn failed_signal_keeps_timer_shutdown_and_successful_signal_stops() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::new(&directory.path().join("timer.sqlite3"));
        let (_request, receive) = watch::channel(false);
        let shutdown = tokio::spawn(wait(
            async {
                Err(io::Error::other(
                    "private marker must never enter diagnostics",
                ))
            },
            receive,
            tokio::time::sleep(Duration::from_secs(10)),
            diagnostics.clone(),
        ));
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        tokio::time::advance(Duration::from_secs(10)).await;
        shutdown.await.unwrap();
        assert!(!serde_json::to_string(&diagnostics.snapshot())
            .unwrap()
            .contains("private marker"));
        let (_request, receive) = watch::channel(false);
        wait(
            async { Ok(()) },
            receive,
            std::future::pending(),
            diagnostics.clone(),
        )
        .await;
        assert_eq!(diagnostics.snapshot().latest.unwrap().sequence, 1);
    }
}
