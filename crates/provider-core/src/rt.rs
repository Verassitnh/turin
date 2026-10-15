//! Shared future and stream boxes for the provider traits.
//!
//! Upstream threads `Effect` and `Stream` through service records. These
//! aliases are the object-safe shape of that: every adapter method that can
//! be absent returns `Option<BoxFuture<_>>` (`None` means the capability is
//! not on the runtime).

use std::{future::Future, pin::Pin};

use futures::Stream;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

pub fn boxed<'a, T: Send + 'a>(future: impl Future<Output = T> + Send + 'a) -> BoxFuture<'a, T> {
    Box::pin(future)
}
