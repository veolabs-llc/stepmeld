//! `stepmeld-run <program> [args...]`: a Batch job's entrypoint
//! (docs/PLACEMENT.md §9). It reads where the Run's folder is from
//! `STEPMELD_RUN` (`s3://bucket/prefix/<run>/`), keeps a local copy for
//! the program (in `STEPMELD_RUN_DIR`, else a temporary folder), runs
//! the program on it as stepmeld runs any program, and keeps the S3
//! folder up to date until the program's outcome is there. Its exit
//! code is the program's. A SIGTERM (Batch terminating the job) is
//! passed on to the program, which is killed if it has not ended a
//! little later.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static TERM: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

fn main() {
    #[cfg(unix)]
    // SAFETY: the handler only stores to an atomic
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let program: Vec<String> = std::env::args().skip(1).collect();
    let url = std::env::var(stepmeld_aws::batch::RUN_ENV).unwrap_or_default();
    let Some((bucket, prefix)) = stepmeld_aws::s3_url(&url) else {
        eprintln!("stepmeld-run: {} is not an s3:// URL: {url:?}", stepmeld_aws::batch::RUN_ENV);
        std::process::exit(2);
    };
    let local = std::env::var_os("STEPMELD_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("stepmeld-run-{}", std::process::id())));
    let settings = stepmeld_aws::Settings {
        region: std::env::var("AWS_REGION").ok().or_else(|| std::env::var("AWS_DEFAULT_REGION").ok()),
        profile: None,
        endpoint: std::env::var("STEPMELD_S3_ENDPOINT").ok(),
    };
    let folders = stepmeld_aws::s3::S3Objects::new(settings.s3(), &bucket);
    match stepmeld_aws::batch::wrapped_until(&folders, &prefix, &program, &local, Duration::from_secs(5), &TERM) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("stepmeld-run: {e}");
            std::process::exit(1);
        }
    }
}
