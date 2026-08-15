//! tests3 — 自 src/cli/tests.rs 拆分。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod default_kb_progress_tests {
    use super::*;

    #[test]
    fn progress_is_emitted_as_a_complete_line() {
        let stage = crate::default_kb::UpdateStage::FetchingRepository;
        let mut output = Vec::new();

        write_default_kb_update_progress(&mut output, stage).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("[default-kb] {}\n", stage.message())
        );
    }
}

/// `gqy reset-memory`:清空当前人格的长期记忆。daemon 在跑走 IPC,
/// 否则本地直清;终端确认后执行。

#[cfg(test)]
mod remote_tool_image_tests {
    use super::*;
    use image::{Delay, Frame, Rgba, RgbaImage};

    #[test]
    fn web_tool_image_event_exposes_asset_id_to_remote_cli() {
        let event = serde_json::json!({
            "run_id": "run-1",
            "name": "show_meme",
            "asset": { "id": "img-1", "mime": "image/gif" }
        });
        assert_eq!(remote_tool_image_asset_id(&event), Some("img-1"));
        assert_eq!(remote_tool_image_asset_id(&serde_json::json!({})), None);
    }

    #[test]
    fn ipc_command_response_distinguishes_errors_and_closed_connections() {
        assert!(validate_ipc_command_response(Some(IpcFrame::Ack)).is_ok());
        let rejected = validate_ipc_command_response(Some(IpcFrame::Error {
            code: None,
            message: "GQY is busy with another operation".to_string(),
        }))
        .unwrap_err();
        assert!(rejected.to_string().contains("busy with another operation"));

        let closed = validate_ipc_command_response(None).unwrap_err();
        assert!(closed
            .to_string()
            .contains("closed the connection without a response"));

        let unexpected = validate_ipc_command_response(Some(IpcFrame::Accepted {
            turn_id: None,
            run_id: "run-test".to_string(),
        }))
        .unwrap_err();
        assert!(unexpected.to_string().contains("unexpected response"));
    }

    #[test]
    fn remote_gif_asset_is_converted_to_static_png() {
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frames((0..2).map(|value| {
                    Frame::from_parts(
                        RgbaImage::from_pixel(32, 32, Rgba([value, 20, 40, 255])),
                        0,
                        0,
                        Delay::from_numer_denom_ms(100, 1),
                    )
                }))
                .unwrap();
        }
        let asset = crate::state::ImageAssetData {
            asset: crate::state::ImageAsset {
                asset_id: "img-gif".to_string(),
                turn_id: "turn-1".to_string(),
                tool_id: Some("tool-1".to_string()),
                mime: "image/gif".to_string(),
                width: 32,
                height: 32,
                alt: "animated meme".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            bytes: gif,
        };
        let preview = remote_image_preview(&asset).unwrap();
        let bytes = std::fs::read(preview.path()).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 32);
    }
}
