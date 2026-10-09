use std::{future::Future, sync::atomic::{AtomicBool, Ordering}, time::Duration};

use anyhow::{Result, bail, ensure};
use iced::futures::future::{Either, select};

pub fn run<T>(cancel: &AtomicBool, future: impl Future<Output = Result<T>>) -> Result<T> {
    ensure!(!cancel.load(Ordering::Relaxed), "Operation cancelled");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let cancellation = async {
            loop {
                if cancel.load(Ordering::Relaxed) { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        match select(Box::pin(future), Box::pin(cancellation)).await {
            Either::Left((result, _)) => result,
            Either::Right(_) => bail!("Operation cancelled"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_drops_pending_network_work_promptly() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            signal.store(true, Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        let result: Result<()> = run(&cancel, std::future::pending());
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        thread.join().unwrap();
    }
}
