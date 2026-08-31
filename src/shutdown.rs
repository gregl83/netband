use std::future::Future;
use std::io;
use std::time::Duration;

use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalName {
    Interrupt,
    Terminate,
}

impl SignalName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedReason {
    SecondSignal(SignalName),
    GraceExpired,
}

#[derive(Debug)]
pub enum Supervised<T> {
    Completed(T),
    Graceful { result: T, signal: SignalName },
    Forced(ForcedReason),
}

pub async fn supervise<T, F>(
    grace: Duration,
    shutdown: watch::Sender<bool>,
    operation: F,
) -> Result<Supervised<T>, io::Error>
where
    F: Future<Output = T>,
{
    let mut signals = SignalListener::new()?;
    tokio::pin!(operation);

    let first = tokio::select! {
        result = &mut operation => return Ok(Supervised::Completed(result)),
        signal = signals.next() => signal,
    };
    tracing::info!(
        signal = first.as_str(),
        "shutdown requested; draining active work"
    );
    let _ = shutdown.send(true);

    tokio::select! {
        result = &mut operation => Ok(Supervised::Graceful { result, signal: first }),
        second = signals.next() => {
            tracing::error!(signal = second.as_str(), "second shutdown signal; forcing termination");
            Ok(Supervised::Forced(ForcedReason::SecondSignal(second)))
        }
        () = tokio::time::sleep(grace) => {
            tracing::error!(grace_ms = grace.as_millis(), "shutdown grace period expired; forcing termination");
            Ok(Supervised::Forced(ForcedReason::GraceExpired))
        }
    }
}

#[cfg(unix)]
struct SignalListener {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl SignalListener {
    fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn next(&mut self) -> SignalName {
        tokio::select! {
            _ = self.interrupt.recv() => SignalName::Interrupt,
            _ = self.terminate.recv() => SignalName::Terminate,
        }
    }
}

#[cfg(not(unix))]
struct SignalListener;

#[cfg(not(unix))]
impl SignalListener {
    fn new() -> io::Result<Self> {
        Ok(Self)
    }

    async fn next(&mut self) -> SignalName {
        let _ = tokio::signal::ctrl_c().await;
        SignalName::Interrupt
    }
}
