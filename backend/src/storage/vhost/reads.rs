//! Shared bounded workers for guest reads. Guest pages are copied only by the queue owner.
use super::{Disk, Request, error};
use std::{
    io,
    sync::{Arc, Mutex, OnceLock, mpsc},
};

pub(super) const CONCURRENCY: usize = 16;

type Outcome = io::Result<(Request, bool)>;

type Job = Box<dyn FnOnce() + Send>;

fn workers() -> io::Result<&'static mpsc::SyncSender<Job>> {
    static WORKERS: OnceLock<Result<mpsc::SyncSender<Job>, String>> = OnceLock::new();
    WORKERS
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Job>(CONCURRENCY);
            let receiver = Arc::new(Mutex::new(receiver));
            for index in 0..CONCURRENCY {
                let receiver = receiver.clone();
                std::thread::Builder::new()
                    .name(format!("leo-disk-read-{index}"))
                    .spawn(move || {
                        loop {
                            let job = match receiver.lock() {
                                Ok(receiver) => receiver.recv(),
                                Err(_) => return,
                            };
                            let Ok(job) = job else { return };
                            job();
                        }
                    })
                    .map_err(|error| error.to_string())?;
            }
            Ok(sender)
        })
        .as_ref()
        .map_err(error)
}

pub(super) fn execute(
    disk: &Arc<dyn Disk>,
    requests: Vec<Request>,
) -> io::Result<Vec<(Request, bool)>> {
    if requests.len() == 1 {
        let mut request = requests.into_iter().next().unwrap();
        let succeeded = disk.read_at(request.offset, &mut request.bytes).is_ok();
        return Ok(vec![(request, succeeded)]);
    }
    let workers = workers()?;
    let count = requests.len();
    let (sender, receiver) = mpsc::channel::<(usize, Outcome)>();
    for (index, mut request) in requests.into_iter().enumerate() {
        let disk = disk.clone();
        let sender = sender.clone();
        workers
            .send(Box::new(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let succeeded = disk.read_at(request.offset, &mut request.bytes).is_ok();
                    (request, succeeded)
                }))
                .map_err(|_| error("Disk read worker panicked"));
                let _ = sender.send((index, outcome));
            }))
            .map_err(error)?;
    }
    drop(sender);
    let mut outcomes: Vec<Option<Outcome>> = (0..count).map(|_| None).collect();
    for (index, outcome) in receiver {
        outcomes[index] = Some(outcome);
    }
    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or_else(|| error("Disk read worker stopped"))?)
        .collect()
}
