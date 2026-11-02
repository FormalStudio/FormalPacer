//! Brings the engine's snapshots to the main thread, one hop per message, as trove's pump does.

use std::rc::Rc;

use nd::prelude::*;

type Apply<T> = Rc<dyn Fn(T, &mut App)>;

pub fn pump<T: Send + 'static>(rx: flume::Receiver<T>, apply: impl Fn(T, &mut App) + 'static) {
    next(rx, Rc::new(apply));
}

fn next<T: Send + 'static>(rx: flume::Receiver<T>, apply: Apply<T>) {
    nd::system::spawn(
        async move {
            let first = rx.recv_async().await.ok()?;
            // Only the newest snapshot matters when several queued up.
            let newest = rx.try_iter().last().unwrap_or(first);
            Some((newest, rx))
        },
        move |out, cx| {
            let Some((value, rx)) = out else { return };
            apply(value, cx);
            next(rx, apply);
        },
    );
}
