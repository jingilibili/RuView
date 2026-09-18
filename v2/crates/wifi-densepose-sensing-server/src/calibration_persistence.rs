//! Persistence of an explicitly completed room calibration.
//!
//! The empty hold costs the operator minutes of an empty room, and a restart -
//! a deploy, a crash, a power cycle - used to discard the completed calibration
//! and demand another hold. This module stores the completed field model with
//! the identity of the hold that produced it, so the server can restore the
//! occupancy authority it already had.
//!
//! A restored image is deliberately narrower than a hold taken in the current
//! process (ADR-364): it may publish occupancy and presence evidence bound to
//! its receipt identity, and it never authorizes numeric vitals, which stay
//! gated on a calibration completed in this process.
//!
//! The image holds aggregate model statistics, configuration, one source node
//! and its grid, and the receipt identity. It excludes raw CSI, waveforms,
//! device addresses, room names, pose, and labels. It is bound to the stable
//! installation ID, digest checked over the exact payload bytes, capped at
//! 1 MiB, and inherits the field model's expiry. Anything that fails to verify
//! is refused, never partially applied.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;
use wifi_densepose_signal::ruvsense::field_model::{
    FieldModel, FieldModelError, FieldModelSnapshotV1,
};

use crate::bootstrap_baseline::{installation_binding, BootstrapCsiGrid};

/// Versioned schema of the persisted explicit calibration image.
pub const EXPLICIT_CALIBRATION_SCHEMA: &str = "ruview.calibration.explicit-field-model.v1";
/// Authority a restored image carries: occupancy evidence, never numeric vitals.
pub const EXPLICIT_CALIBRATION_AUTHORITY: &str = "explicit_calibration_restored";
const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_ID_CHARS: usize = 128;
const MAX_SOURCE_NODES: usize = 8;

/// Identity of the hold that produced the stored model, as the server received
/// it at finalization. Restoring an image reproduces this identity exactly, so
/// a consumer can match the evidence it reads after a restart against the
/// receipt it accepted before one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitCalibrationIdentity {
    pub boot_epoch: String,
    pub session_id: String,
    pub model_id: String,
    pub binding_digest: String,
    pub source_node_ids: Vec<u8>,
    pub source_grid: BootstrapCsiGrid,
    pub frame_count: u64,
    pub variance_explained: f64,
    pub baseline_eigenvalue_count: usize,
    pub model_completed_at_unix_ms: u64,
}

/// What a stored image reports about itself, without the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplicitCalibrationMetadata {
    pub authority: &'static str,
    pub identity: ExplicitCalibrationIdentity,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExplicitPayloadV1 {
    schema: String,
    authority: String,
    installation_binding_sha256: String,
    identity: ExplicitCalibrationIdentity,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    field_model: FieldModelSnapshotV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ExplicitImageV1 {
    payload: ExplicitPayloadV1,
    content_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplicitImageRawV1 {
    payload: Box<RawValue>,
    content_sha256: String,
}

#[derive(Debug, Error)]
pub enum ExplicitCalibrationError {
    #[error("a stable installation_id is required for local calibration persistence")]
    MissingInstallationId,
    #[error("stored calibration path is invalid")]
    InvalidPath,
    #[error("stored calibration file is too large")]
    FileTooLarge,
    #[error("stored calibration image is malformed: {0}")]
    Malformed(String),
    #[error("stored calibration belongs to another installation")]
    InstallationMismatch,
    #[error("stored calibration has expired")]
    Expired,
    #[error("stored calibration digest does not verify")]
    DigestMismatch,
    #[error("field model snapshot is invalid: {0}")]
    FieldModel(#[from] FieldModelError),
    #[error("stored calibration storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored calibration serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir
        .join("calibration")
        .join("explicit-field-model-v1.json")
}

/// Persist a completed calibration. Returns what the image reports about itself.
pub fn store(
    path: &Path,
    installation_id: &str,
    identity: &ExplicitCalibrationIdentity,
    field_model: &FieldModel,
    created_at_unix_ms: u64,
) -> Result<ExplicitCalibrationMetadata, ExplicitCalibrationError> {
    let snapshot = field_model.export_snapshot()?;
    let calibrated_at_ms = snapshot.modes.calibrated_at_us / 1_000;
    let expiry_ms = (snapshot.config.baseline_expiry_s * 1_000.0) as u64;
    let payload = ExplicitPayloadV1 {
        schema: EXPLICIT_CALIBRATION_SCHEMA.to_string(),
        authority: EXPLICIT_CALIBRATION_AUTHORITY.to_string(),
        installation_binding_sha256: installation_binding(installation_id)
            .map_err(|_| ExplicitCalibrationError::MissingInstallationId)?,
        identity: identity.clone(),
        created_at_unix_ms,
        expires_at_unix_ms: calibrated_at_ms.saturating_add(expiry_ms),
        field_model: snapshot,
    };
    validate_payload(&payload, installation_id, created_at_unix_ms)?;
    let image = ExplicitImageV1 {
        content_sha256: payload_digest(&payload)?,
        payload,
    };
    let bytes = serde_json::to_vec(&image)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ExplicitCalibrationError::FileTooLarge);
    }
    atomic_write_private(path, &bytes)?;
    Ok(metadata(&image))
}

/// Load a stored calibration for the given installation. Anything that does not
/// verify - schema, binding, digest, size, lifetime, snapshot - is refused.
pub fn load(
    path: &Path,
    installation_id: &str,
    current_unix_ms: u64,
) -> Result<(FieldModel, ExplicitCalibrationMetadata), ExplicitCalibrationError> {
    let file_metadata = fs::symlink_metadata(path)?;
    if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
        return Err(ExplicitCalibrationError::InvalidPath);
    }
    if file_metadata.len() > MAX_FILE_BYTES {
        return Err(ExplicitCalibrationError::FileTooLarge);
    }
    let mut bytes = Vec::with_capacity(file_metadata.len() as usize);
    OpenOptions::new()
        .read(true)
        .open(path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ExplicitCalibrationError::FileTooLarge);
    }
    // Verify the exact payload bytes written by storage before decoding the
    // floating point model. Equivalent JSON number spellings may serialize
    // differently, so decode and reserialize is not an integrity contract.
    let raw_image: ExplicitImageRawV1 = serde_json::from_slice(&bytes)?;
    if hex_sha256(raw_image.payload.get().as_bytes()) != raw_image.content_sha256 {
        return Err(ExplicitCalibrationError::DigestMismatch);
    }
    let payload: ExplicitPayloadV1 = serde_json::from_str(raw_image.payload.get())?;
    validate_payload(&payload, installation_id, current_unix_ms)?;
    let image = ExplicitImageV1 {
        payload,
        content_sha256: raw_image.content_sha256,
    };
    let model = FieldModel::from_snapshot(
        image.payload.field_model.clone(),
        current_unix_ms.saturating_mul(1_000),
    )?;
    Ok((model, metadata(&image)))
}

pub fn remove(path: &Path) -> Result<bool, ExplicitCalibrationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ExplicitCalibrationError::InvalidPath);
            }
            fs::remove_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn metadata(image: &ExplicitImageV1) -> ExplicitCalibrationMetadata {
    ExplicitCalibrationMetadata {
        authority: EXPLICIT_CALIBRATION_AUTHORITY,
        identity: image.payload.identity.clone(),
        created_at_unix_ms: image.payload.created_at_unix_ms,
        expires_at_unix_ms: image.payload.expires_at_unix_ms,
        content_sha256: image.content_sha256.clone(),
    }
}

fn validate_payload(
    payload: &ExplicitPayloadV1,
    installation_id: &str,
    current_unix_ms: u64,
) -> Result<(), ExplicitCalibrationError> {
    let malformed = |field: &str| ExplicitCalibrationError::Malformed(field.to_string());
    if payload.schema != EXPLICIT_CALIBRATION_SCHEMA {
        return Err(malformed("schema"));
    }
    if payload.authority != EXPLICIT_CALIBRATION_AUTHORITY {
        return Err(malformed("authority"));
    }
    if payload.installation_binding_sha256
        != installation_binding(installation_id)
            .map_err(|_| ExplicitCalibrationError::MissingInstallationId)?
    {
        return Err(ExplicitCalibrationError::InstallationMismatch);
    }

    let identity = &payload.identity;
    for (label, value) in [
        ("boot_epoch", &identity.boot_epoch),
        ("session_id", &identity.session_id),
        ("model_id", &identity.model_id),
        ("binding_digest", &identity.binding_digest),
    ] {
        if value.trim().is_empty() || value.len() > MAX_ID_CHARS {
            return Err(malformed(label));
        }
    }
    if identity.source_node_ids.is_empty() || identity.source_node_ids.len() > MAX_SOURCE_NODES {
        return Err(malformed("source_node_ids"));
    }
    if identity.source_grid.n_subcarriers == 0 {
        return Err(malformed("source_grid"));
    }
    if identity.frame_count == 0 {
        return Err(malformed("frame_count"));
    }
    if !identity.variance_explained.is_finite() {
        return Err(malformed("variance_explained"));
    }

    // The image may not outlive the model it carries, and its lifetime is the
    // model's own: a stored calibration is never fresher than its snapshot.
    let calibrated_at_ms = payload.field_model.modes.calibrated_at_us / 1_000;
    let expiry_ms = (payload.field_model.config.baseline_expiry_s * 1_000.0) as u64;
    if payload.expires_at_unix_ms != calibrated_at_ms.saturating_add(expiry_ms) {
        return Err(malformed("expires_at_unix_ms"));
    }
    if payload.created_at_unix_ms < identity.model_completed_at_unix_ms {
        return Err(malformed("created_at_unix_ms"));
    }
    if payload.expires_at_unix_ms <= payload.created_at_unix_ms {
        return Err(malformed("lifetime"));
    }
    if current_unix_ms >= payload.expires_at_unix_ms {
        return Err(ExplicitCalibrationError::Expired);
    }
    Ok(())
}

fn payload_digest(payload: &ExplicitPayloadV1) -> Result<String, ExplicitCalibrationError> {
    Ok(hex_sha256(&serde_json::to_vec(payload)?))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), ExplicitCalibrationError> {
    let parent = path.parent().ok_or(ExplicitCalibrationError::InvalidPath)?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("calibration"),
        std::process::id()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wifi_densepose_signal::ruvsense::field_model::FieldModelConfig;

    const HOUR_MS: u64 = 3_600_000;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_millis() as u64
    }

    fn finalized_model(calibrated_at_unix_ms: u64) -> FieldModel {
        let mut model = FieldModel::new(FieldModelConfig {
            n_links: 1,
            n_subcarriers: 56,
            n_modes: 3,
            min_calibration_frames: 10,
            min_calibration_duration_s: 0.0,
            baseline_expiry_s: (HOUR_MS as f64) / 1_000.0,
        })
        .expect("config");
        for frame_index in 0..20 {
            let frame = (0..56)
                .map(|subcarrier| {
                    10.0 + subcarrier as f64 * 0.01
                        + (frame_index as f64 * 0.1 + subcarrier as f64 * 0.03).sin() * 0.01
                })
                .collect();
            model.feed_calibration(&[frame]).expect("feed");
        }
        model
            .finalize_calibration(calibrated_at_unix_ms.saturating_mul(1_000), 7)
            .expect("finalize");
        model
    }

    fn identity(completed_at_unix_ms: u64) -> ExplicitCalibrationIdentity {
        ExplicitCalibrationIdentity {
            boot_epoch: "cal-boot-test".to_string(),
            session_id: "cal-session-test".to_string(),
            model_id: "cal-model-test".to_string(),
            binding_digest: "ab".repeat(32),
            source_node_ids: vec![5],
            source_grid: BootstrapCsiGrid {
                n_subcarriers: 64,
                ppdu_type: 0,
            },
            frame_count: 1_000,
            variance_explained: 0.9,
            baseline_eigenvalue_count: 1,
            model_completed_at_unix_ms: completed_at_unix_ms,
        }
    }

    fn temp_path(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("ruview-calibration-{label}-{unique}"))
            .join("explicit-field-model-v1.json")
    }

    fn store_test_image(path: &Path) -> (FieldModel, ExplicitCalibrationMetadata) {
        let now = now_ms();
        let model = finalized_model(now);
        let metadata = store(path, "install-a", &identity(now), &model, now).expect("store");
        (model, metadata)
    }

    #[test]
    fn round_trip_restores_the_model_with_its_receipt_identity() {
        let path = temp_path("round-trip");
        let (_, stored) = store_test_image(&path);

        let (restored, loaded) = load(&path, "install-a", now_ms()).expect("load");
        assert_eq!(loaded.authority, EXPLICIT_CALIBRATION_AUTHORITY);
        assert_eq!(loaded.identity, stored.identity);
        assert_eq!(loaded.content_sha256, stored.content_sha256);
        assert_eq!(
            restored.export_snapshot().expect("snapshot").modes.calibrated_at_us,
            stored.identity.model_completed_at_unix_ms * 1_000
        );
        assert!(remove(&path).expect("remove"));
        assert!(!path.exists());
    }

    #[test]
    fn another_installation_cannot_restore_the_image() {
        let path = temp_path("installation");
        store_test_image(&path);
        let error = load(&path, "install-b", now_ms()).expect_err("installation mismatch");
        assert!(matches!(
            error,
            ExplicitCalibrationError::InstallationMismatch
        ));
        remove(&path).expect("remove");
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let path = temp_path("tamper");
        store_test_image(&path);
        let mut bytes = fs::read(&path).expect("read");
        let marker = b"cal-model-test";
        let at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("model id present");
        bytes[at] = b'X';
        fs::write(&path, &bytes).expect("write");

        let error = load(&path, "install-a", now_ms()).expect_err("digest mismatch");
        assert!(matches!(error, ExplicitCalibrationError::DigestMismatch));
        remove(&path).expect("remove");
    }

    #[test]
    fn an_expired_image_is_refused() {
        let path = temp_path("expiry");
        let (_, stored) = store_test_image(&path);
        let error = load(&path, "install-a", stored.expires_at_unix_ms + 1)
            .expect_err("expired image");
        assert!(matches!(error, ExplicitCalibrationError::Expired));
        remove(&path).expect("remove");
    }

    #[test]
    fn an_oversized_file_is_refused() {
        let path = temp_path("oversize");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, vec![b'{'; (MAX_FILE_BYTES + 1) as usize]).expect("write");
        let error = load(&path, "install-a", now_ms()).expect_err("oversized file");
        assert!(matches!(error, ExplicitCalibrationError::FileTooLarge));
        fs::remove_file(&path).expect("remove file");
    }
}
