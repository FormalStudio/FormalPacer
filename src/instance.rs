//! One FormalPacer per session: a second launch hands over to the running one, which shows its
//! window, and exits.

use nd::prelude::*;

use crate::Model;

pub enum Claim {
    Primary(nd_ipc::instance::Primary),
    Forwarded,
}

fn runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RT: std::sync::OnceLock<Option<tokio::runtime::Runtime>> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_current_thread().enable_all().build().ok()).as_ref()
}

/// `None` when the lock cannot be taken at all; the app then runs anyway.
pub fn claim() -> Option<Claim> {
    let endpoint = nd_ipc::Endpoint::for_app("formalpacer", "instance");
    let launch = nd_ipc::instance::Launch::from_env();
    match runtime()?.block_on(nd_ipc::instance::acquire(&endpoint, &launch)) {
        Ok(nd_ipc::instance::Instance::Primary(primary)) => Some(Claim::Primary(primary)),
        Ok(nd_ipc::instance::Instance::Forwarded) => Some(Claim::Forwarded),
        Err(e) => {
            nd::report_issue(&format!("single instance: {e}"));
            None
        }
    }
}

pub fn show_on_launch(mut primary: nd_ipc::instance::Primary, model: Entity<Model>) {
    let Some(rt) = runtime() else { return };
    let (tx, rx) = flume::unbounded();
    std::thread::Builder::new()
        .name("formalpacer-instance".into())
        .spawn(move || {
            rt.block_on(async move {
                while primary.next_launch().await.is_some() {
                    if tx.send(()).is_err() {
                        break;
                    }
                }
            });
        })
        .ok();
    crate::pump::pump(rx, move |(), cx| crate::show(cx, &model));
}
