//! Video export helpers: how project export formats map onto FFmpeg codecs and containers, and how the
//! keyframe interval is chosen for seamless loops.

use loopcut_core::{EncoderBackend, ExportFormat, VideoCodec};
use loopcut_media::encode::Container;

/// Longest keyframe interval requested from the video encoders, in frames.
pub const MAX_KEYINT: u32 = 600;

/// Codec and container for a video export format, or `None` for GIF (which has its own engines).
pub fn video_target(format: ExportFormat) -> Option<(VideoCodec, Container)> {
    match format {
        ExportFormat::Gif => None,
        ExportFormat::Mp4H264 => Some((VideoCodec::H264, Container::Mp4)),
        ExportFormat::Mp4Hevc => Some((VideoCodec::Hevc, Container::Mp4)),
        ExportFormat::WebmVp9 => Some((VideoCodec::Vp9, Container::Webm)),
        ExportFormat::WebmAv1 => Some((VideoCodec::Av1, Container::Webm)),
    }
}

/// The encoder that runs for `codec` when the user asked for `requested`.
///
/// No GPU encoder exists for VP9, so VP9 always uses libvpx-vp9 whatever the request says. Every
/// other codec keeps the requested backend.
pub fn encoder_for(codec: VideoCodec, requested: EncoderBackend) -> EncoderBackend {
    match codec {
        VideoCodec::Vp9 => EncoderBackend::Cpu,
        _ => requested,
    }
}

/// Keyframe interval for a seamless loop: one keyframe per loop, so the first frame of every
/// repetition is a keyframe. `frames` is the plan's frame count; the result is capped at
/// [`MAX_KEYINT`] and is never zero.
pub fn keyint_for_loop(frames: usize) -> u32 {
    u32::try_from(frames)
        .unwrap_or(MAX_KEYINT)
        .clamp(1, MAX_KEYINT)
}

/// Short name of an encoder backend for events and logs.
pub fn backend_label(backend: EncoderBackend) -> &'static str {
    match backend {
        EncoderBackend::Cpu => "cpu",
        EncoderBackend::Nvenc { .. } => "nvenc",
        EncoderBackend::Amf { .. } => "amf",
        EncoderBackend::Qsv { .. } => "qsv",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_map_to_codecs_and_containers() {
        assert_eq!(video_target(ExportFormat::Gif), None);
        assert_eq!(
            video_target(ExportFormat::Mp4H264),
            Some((VideoCodec::H264, Container::Mp4))
        );
        assert_eq!(
            video_target(ExportFormat::Mp4Hevc),
            Some((VideoCodec::Hevc, Container::Mp4))
        );
        assert_eq!(
            video_target(ExportFormat::WebmVp9),
            Some((VideoCodec::Vp9, Container::Webm))
        );
        assert_eq!(
            video_target(ExportFormat::WebmAv1),
            Some((VideoCodec::Av1, Container::Webm))
        );
    }

    #[test]
    fn vp9_always_runs_on_the_cpu() {
        let amf = EncoderBackend::Amf { dxgi_index: 0 };
        assert_eq!(encoder_for(VideoCodec::Vp9, amf), EncoderBackend::Cpu);
        assert_eq!(encoder_for(VideoCodec::Hevc, amf), amf);
    }

    #[test]
    fn keyint_is_the_loop_length_capped() {
        assert_eq!(keyint_for_loop(0), 1);
        assert_eq!(keyint_for_loop(18), 18);
        assert_eq!(keyint_for_loop(5000), MAX_KEYINT);
    }

    #[test]
    fn backend_labels_are_stable() {
        assert_eq!(backend_label(EncoderBackend::Cpu), "cpu");
        assert_eq!(
            backend_label(EncoderBackend::Nvenc { cuda_index: 0 }),
            "nvenc"
        );
    }
}
