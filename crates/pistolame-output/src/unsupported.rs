//! Backend for platforms with no output backend (the BSDs; macOS and Linux used it in M1, plan
//! B5.9). It has no devices, so the app can say "no audio output on this system yet" while the
//! workspace keeps compiling and testing on every OS. It contains no platform code, so it is
//! compiled everywhere and its tests run on every OS.

use crate::{
    AudioOutput, DeviceSampleFormat, FormatProbeResult, OpenStreamError, OpenStreamRequest,
    OutputDeviceId, OutputDeviceInfo, OutputError, OutputStream, RenderSource,
};

/// An `AudioOutput` with no devices: listing returns nothing and opening always fails with
/// `DeviceNotFound`.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnsupportedPlatformOutput;

impl AudioOutput for UnsupportedPlatformOutput {
    fn backend_name(&self) -> &'static str {
        "none"
    }

    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError> {
        Ok(Vec::new())
    }

    fn probe_formats(
        &mut self,
        _device_id: &OutputDeviceId,
        _candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError> {
        Err(OutputError::DeviceNotFound)
    }

    fn open_stream(
        &mut self,
        _request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        Err(OpenStreamError {
            error: OutputError::DeviceNotFound,
            render_source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelMask, OpenPolicy, RenderOutcome, RenderStreamDescription, StreamFormat};

    struct SilentRenderSource;

    impl RenderSource for SilentRenderSource {
        fn begin_stream(&mut self, _description: RenderStreamDescription) {}

        fn render(&mut self, destination: &mut [u8]) -> RenderOutcome {
            destination.fill(0);
            RenderOutcome::EndOfStream
        }
    }

    #[test]
    fn unsupported_output_lists_no_device() {
        let mut output = UnsupportedPlatformOutput;
        assert!(output.list_devices().unwrap().is_empty());
    }

    #[test]
    fn unsupported_output_refuses_to_open_and_hands_the_render_source_back() {
        let mut output = UnsupportedPlatformOutput;
        let request = OpenStreamRequest {
            device_id: OutputDeviceId("any".to_owned()),
            source_format: StreamFormat {
                sample_rate_hz: 44_100,
                bits_per_sample: 16,
                channel_count: 2,
                channel_mask: ChannelMask(0x3),
            },
            policy: OpenPolicy::ExclusiveOnly,
        };
        let Err(open_error) = output.open_stream(request, Box::new(SilentRenderSource)) else {
            panic!("the unsupported backend must not open a stream");
        };
        assert!(matches!(open_error.error, OutputError::DeviceNotFound));
        let mut returned_source = open_error.render_source;
        let mut destination = [1u8; 4];
        assert_eq!(
            returned_source.render(&mut destination),
            RenderOutcome::EndOfStream
        );
    }
}
