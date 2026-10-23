//! Helpers shared by the fuzz targets.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Runs a future that reads from memory: it never has to wait, so one poll
/// finishes it and no runtime is needed.
pub fn now<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("reading from memory never waits"),
    }
}
