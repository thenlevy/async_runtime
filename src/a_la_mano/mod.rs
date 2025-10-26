mod executor;
mod reactor;

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

struct Timer {
    fires_at: Instant,
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if Instant::now() >= self.fires_at {
            Poll::Ready(())
        } else {
            // Pending, but we must promise to call cx.waker().wake() when the timer would fire.
            Poll::Pending
        }
    }
}
