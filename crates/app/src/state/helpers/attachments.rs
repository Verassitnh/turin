//! Port of upstream/packages/client-runtime/src/state/attachments.ts
//!
//! Only the attachment limits and labels. The signed-URL upload cycle
//! (`runAttachmentUploadCycle`, `verifyPersistedAttachmentUpload`,
//! `deletePendingAttachmentUpload`, `isAssetAttachmentNotFoundFailure`) and
//! its RPC commands are not ported: the composer hands bytes to the
//! orchestrator in-process (`CommandPort::persist_chat_attachments`), so there
//! is no upload URL to mint, resolve or expire.

use turin_contracts::chat_attachment::PROVIDER_SEND_TURN_MAX_FILE_BYTES;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// The effective per-file byte limit for a server that advertises
/// `capabilities.fileAttachments.maxUploadBytes`. The contract caps what a
/// turn may reference, so a larger advertised value must not admit files the
/// send would then refuse.
pub fn clamp_file_attachment_upload_bytes(advertised_max_upload_bytes: u64) -> u64 {
    advertised_max_upload_bytes.min(PROVIDER_SEND_TURN_MAX_FILE_BYTES)
}

/// "3.2 MB" / "48 KB" label for attachment rows. Never shows "0 KB".
pub fn format_attachment_size(size_bytes: u64) -> String {
    if size_bytes >= MIB {
        format!("{:.1} MB", size_bytes as f64 / MIB as f64)
    } else {
        format!("{} KB", size_bytes.div_ceil(KIB).max(1))
    }
}

/// User-facing rejection for a file over the effective upload limit.
pub fn file_attachment_too_large_message(name: &str, max_upload_bytes: u64) -> String {
    let max_upload_size = if max_upload_bytes >= MIB && max_upload_bytes.is_multiple_of(MIB) {
        format!("{} MB", max_upload_bytes / MIB)
    } else if max_upload_bytes >= KIB && max_upload_bytes.is_multiple_of(KIB) {
        format!("{} KB", max_upload_bytes / KIB)
    } else {
        format!(
            "{max_upload_bytes} {}",
            if max_upload_bytes == 1 {
                "byte"
            } else {
                "bytes"
            }
        )
    };
    format!("'{name}' exceeds the {max_upload_size} attachment limit.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_the_advertised_limit_to_the_turn_contract_cap() {
        assert_eq!(clamp_file_attachment_upload_bytes(1024), 1024);
        assert_eq!(
            clamp_file_attachment_upload_bytes(PROVIDER_SEND_TURN_MAX_FILE_BYTES * 2),
            PROVIDER_SEND_TURN_MAX_FILE_BYTES
        );
    }

    #[test]
    fn formats_attachment_row_sizes() {
        assert_eq!(format_attachment_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(format_attachment_size(1), "1 KB");
    }

    #[test]
    fn formats_small_upload_limits_without_rounding_them_to_zero_mb() {
        assert_eq!(
            file_attachment_too_large_message("tiny.txt", 1),
            "'tiny.txt' exceeds the 1 byte attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("small.txt", 1024),
            "'small.txt' exceeds the 1 KB attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("exact.txt", 1025),
            "'exact.txt' exceeds the 1025 bytes attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("medium.zip", 512 * 1024),
            "'medium.zip' exceeds the 512 KB attachment limit."
        );
    }

    #[test]
    fn keeps_whole_mb_upload_limits_for_standard_server_caps() {
        assert_eq!(
            file_attachment_too_large_message("one.bin", 1024 * 1024),
            "'one.bin' exceeds the 1 MB attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("big.zip", 50 * 1024 * 1024),
            "'big.zip' exceeds the 50 MB attachment limit."
        );
    }
}
