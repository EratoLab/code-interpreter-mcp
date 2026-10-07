use anyhow::{Result, anyhow, bail};
use facet::Facet;
use serde::Deserialize;

pub const MAX_CODE_BYTES: usize = 1024 * 1024;
pub const MAX_FILES: usize = 64;
pub const BYTES_PER_KB: usize = 1024;
const MAX_CONFIGURED_FILE_KB: usize = 1024 * 1024;
const HTTP_HEADROOM_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Facet)]
#[serde(default)]
pub struct InputLimitsConfig {
    /// Maximum decoded size of one input file in KB (1 KB = 1024 bytes).
    pub max_file_kb: usize,
    /// Maximum decoded size across input files in KB (1 KB = 1024 bytes).
    pub max_total_file_kb: usize,
}

impl Default for InputLimitsConfig {
    fn default() -> Self {
        Self {
            max_file_kb: 10 * 1024,
            max_total_file_kb: 50 * 1024,
        }
    }
}

/// Validated limits shared by tool validation and the HTTP transport.
#[derive(Debug, Clone, Copy)]
pub struct InputLimits {
    max_file_bytes: usize,
    max_total_file_bytes: usize,
    max_encoded_file_bytes: usize,
    max_request_body_bytes: usize,
}

fn base64_bytes(decoded_bytes: usize) -> Option<usize> {
    decoded_bytes.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

impl InputLimitsConfig {
    pub fn validate(&self) -> Result<InputLimits> {
        for (name, kb) in [
            ("max_file_kb", self.max_file_kb),
            ("max_total_file_kb", self.max_total_file_kb),
        ] {
            if kb == 0 || kb > MAX_CONFIGURED_FILE_KB {
                bail!(
                    "input_limits.{name} must be between 1 and {MAX_CONFIGURED_FILE_KB} KB (1 KB = {BYTES_PER_KB} decoded bytes)"
                );
            }
        }
        if self.max_file_kb > self.max_total_file_kb {
            bail!("input_limits.max_file_kb must not exceed input_limits.max_total_file_kb");
        }
        let overflow =
            || anyhow!("input_limits KB conversion, base64 expansion or HTTP headroom overflowed");
        let max_file_bytes = self
            .max_file_kb
            .checked_mul(BYTES_PER_KB)
            .ok_or_else(overflow)?;
        let max_total_file_bytes = self
            .max_total_file_kb
            .checked_mul(BYTES_PER_KB)
            .ok_or_else(overflow)?;
        let max_encoded_file_bytes = base64_bytes(max_file_bytes).ok_or_else(overflow)?;
        // Each file is padded separately. Reserving four extra bytes for each
        // additional file bounds the sum even for 64 tiny, padded files.
        let max_request_body_bytes = base64_bytes(max_total_file_bytes)
            .and_then(|bytes| bytes.checked_add((MAX_FILES - 1).checked_mul(4)?))
            .and_then(|bytes| bytes.checked_add(HTTP_HEADROOM_BYTES))
            .ok_or_else(overflow)?;
        Ok(InputLimits {
            max_file_bytes,
            max_total_file_bytes,
            max_encoded_file_bytes,
            max_request_body_bytes,
        })
    }
}

impl InputLimits {
    pub fn max_file_bytes(self) -> usize {
        self.max_file_bytes
    }

    pub fn max_total_file_bytes(self) -> usize {
        self.max_total_file_bytes
    }

    pub fn max_encoded_file_bytes(self) -> usize {
        self.max_encoded_file_bytes
    }

    pub fn max_request_body_bytes(self) -> usize {
        self.max_request_body_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_http_limit_with_independent_padding_and_json_headroom() {
        let defaults = InputLimitsConfig::default().validate().unwrap();
        assert_eq!(defaults.max_file_bytes(), 10 * 1024 * 1024);
        assert_eq!(defaults.max_total_file_bytes(), 50 * 1024 * 1024);
        assert_eq!(defaults.max_request_body_bytes(), 78_293_928);
        for total_kb in [1, 2, 3, 63, 64, 65, 100, MAX_CONFIGURED_FILE_KB] {
            let limits = InputLimitsConfig {
                max_file_kb: total_kb,
                max_total_file_kb: total_kb,
            }
            .validate()
            .unwrap();
            let total = total_kb * BYTES_PER_KB;
            assert_eq!(limits.max_file_bytes(), total);
            assert_eq!(limits.max_total_file_bytes(), total);
            let file_count = total.min(MAX_FILES);
            let first_file = total - (file_count - 1);
            let independently_encoded = base64_bytes(first_file).unwrap() + (file_count - 1) * 4;
            assert!(limits.max_request_body_bytes() >= independently_encoded + HTTP_HEADROOM_BYTES);
        }
        assert!(base64_bytes(usize::MAX).is_none());
    }

    #[test]
    fn rejects_zero_unbounded_and_inconsistent_limits() {
        for (file, total) in [
            (0, 1),
            (1, 0),
            (2, 1),
            (MAX_CONFIGURED_FILE_KB + 1, MAX_CONFIGURED_FILE_KB + 1),
            (1, MAX_CONFIGURED_FILE_KB + 1),
            (usize::MAX, usize::MAX),
        ] {
            assert!(
                InputLimitsConfig {
                    max_file_kb: file,
                    max_total_file_kb: total,
                }
                .validate()
                .is_err(),
                "file={file}, total={total}"
            );
        }
        assert!(
            InputLimitsConfig {
                max_file_kb: MAX_CONFIGURED_FILE_KB,
                max_total_file_kb: MAX_CONFIGURED_FILE_KB,
            }
            .validate()
            .is_ok()
        );
    }
}
