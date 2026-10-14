use std::{env::VarError, num::NonZeroUsize};

const ENV_WORKER_THREADS: &str = "TOKIO_WORKER_THREADS";
const MIN_WORKER_THREADS: usize = 4;

pub(crate) fn configured_worker_threads() -> usize {
    worker_threads(std::env::var(ENV_WORKER_THREADS), || {
        std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
    })
}

// Match Erato's runtime sizing: small Kubernetes CPU limits must not reduce
// the server to one Tokio worker. Explicit environment overrides still win.
fn worker_threads(value: Result<String, VarError>, parallelism: impl FnOnce() -> usize) -> usize {
    match value {
        Ok(value) => {
            let count: usize = value.parse().unwrap_or_else(|error| {
                panic!("\"{ENV_WORKER_THREADS}\" must be usize, error: {error}, value: {value}")
            });
            assert!(count > 0, "\"{ENV_WORKER_THREADS}\" cannot be set to 0");
            if count < MIN_WORKER_THREADS {
                eprintln!(
                    "Warning: \"{ENV_WORKER_THREADS}\" is set to {count}, which is below the recommended minimum of {MIN_WORKER_THREADS} worker threads."
                );
            }
            count
        }
        Err(VarError::NotPresent) => parallelism().max(MIN_WORKER_THREADS),
        Err(VarError::NotUnicode(value)) => {
            panic!("\"{ENV_WORKER_THREADS}\" must be valid unicode, error: {value:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detection_has_a_floor_and_preserves_larger_counts() {
        for (available, expected) in [(1, 4), (2, 4), (4, 4), (12, 12)] {
            assert_eq!(
                worker_threads(Err(VarError::NotPresent), || available),
                expected
            );
        }
    }

    #[test]
    fn explicit_overrides_win_without_detecting_parallelism() {
        for count in [1, 2, 4, 12] {
            assert_eq!(
                worker_threads(Ok(count.to_string()), || panic!(
                    "unexpected auto-detection"
                )),
                count
            );
        }
    }

    #[test]
    fn invalid_overrides_are_rejected() {
        for value in [
            "0",
            "",
            "invalid",
            "-1",
            "1.5",
            "999999999999999999999999999999",
        ] {
            assert!(std::panic::catch_unwind(|| worker_threads(Ok(value.into()), || 1)).is_err());
        }
        assert!(
            std::panic::catch_unwind(|| {
                worker_threads(Err(VarError::NotUnicode("invalid".into())), || 1)
            })
            .is_err()
        );
    }
}
