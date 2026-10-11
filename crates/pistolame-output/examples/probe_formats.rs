//! Lists the output devices and asks each one which lossless candidate formats it accepts in
//! WASAPI exclusive mode, at the common sample rates, without playing anything (plan B10.5).
//! It also shows the device periods and the shared-mode mix format (`GetMixFormat`), the format
//! the Windows audio engine converts shared-mode streams to (plan B5.10).
//!
//! Usage: `cargo run -p pistolame-output --example probe_formats [-- --device "K11"]`
//! `--device` keeps only devices whose name contains the text (case-insensitive).
//!
//! Output is plain text meant to be pasted back into an issue or a chat.

#![allow(clippy::print_stdout, reason = "command-line diagnostic tool")]

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows_probe::run()
}

#[cfg(not(windows))]
fn main() {
    println!("probe_formats: only the WASAPI backend exists so far; nothing to probe on this OS.");
}

#[cfg(windows)]
mod windows_probe {
    use std::process::ExitCode;

    use pistolame_output::wasapi::{ExclusiveCandidateProbe, WasapiOutput};
    use pistolame_output::{
        AudioOutput, ChannelMask, FormatProbeOutcome, OutputDeviceInfo, StreamFormat,
        ValidBitsAlignment, lossless_device_format_candidates,
    };

    /// Rates probed (plan B10.5).
    const PROBED_SAMPLE_RATES_HZ: [u32; 8] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];
    /// Source depths probed; each gives its own candidate list.
    const PROBED_SOURCE_BITS: [u8; 2] = [16, 24];
    /// Channels probed: stereo with the default mask (front left and right).
    const PROBED_CHANNEL_COUNT: u8 = 2;
    /// REFERENCE_TIME ticks (100 ns) per millisecond.
    const HUNDRED_NANOSECONDS_PER_MILLISECOND: f64 = 10_000.0;
    /// Width of one result column.
    const COLUMN_WIDTH: usize = 12;

    pub(super) fn run() -> ExitCode {
        let device_name_filter = device_name_filter_from_arguments();
        let mut output = WasapiOutput::new();
        let devices = match output.list_devices() {
            Ok(devices) => devices,
            Err(error) => {
                println!("Could not list output devices: {error}");
                return ExitCode::FAILURE;
            }
        };
        let selected: Vec<&OutputDeviceInfo> = devices
            .iter()
            .filter(|device| match &device_name_filter {
                Some(filter) => device.display_name.to_lowercase().contains(filter),
                None => true,
            })
            .collect();
        println!(
            "Pistolame probe_formats (WASAPI), {} output device(s) listed",
            devices.len()
        );
        if selected.is_empty() {
            println!("No device matches the --device filter.");
            return ExitCode::FAILURE;
        }
        for device in selected {
            print_device_report(&mut output, device);
        }
        print_legend();
        ExitCode::SUCCESS
    }

    fn device_name_filter_from_arguments() -> Option<String> {
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            if argument == "--device" {
                return arguments.next().map(|name| name.to_lowercase());
            }
        }
        None
    }

    fn print_device_report(output: &mut WasapiOutput, device: &OutputDeviceInfo) {
        println!();
        println!(
            "=== {}{}",
            device.display_name,
            if device.is_system_default {
                "  [system default]"
            } else {
                ""
            }
        );
        println!("id: {}", device.id.0);
        match output.device_periods(&device.id) {
            Ok(periods) => println!(
                "device period: default {:.3} ms, minimum {:.3} ms",
                periods.default_period_in_hundred_nanoseconds as f64
                    / HUNDRED_NANOSECONDS_PER_MILLISECOND,
                periods.minimum_period_in_hundred_nanoseconds as f64
                    / HUNDRED_NANOSECONDS_PER_MILLISECOND
            ),
            Err(error) => println!("device period: error: {error}"),
        }
        match output.system_mix_format(&device.id) {
            Ok(mix) => println!(
                "shared-mode mix format: {} Hz, {}-bit {}, {} channels",
                mix.sample_rate_hz,
                mix.bits_per_sample,
                if mix.is_float { "float" } else { "integer" },
                mix.channel_count
            ),
            Err(error) => println!("shared-mode mix format: error: {error}"),
        }
        for source_bits in PROBED_SOURCE_BITS {
            print_exclusive_table(output, device, source_bits);
        }
    }

    fn print_exclusive_table(
        output: &mut WasapiOutput,
        device: &OutputDeviceInfo,
        source_bits: u8,
    ) {
        println!();
        println!("exclusive mode, {source_bits}-bit source, stereo mask 0x3 (IsFormatSupported)");
        let mut header = format!("{:>10}", "rate Hz");
        let header_source = stereo_source(source_bits, PROBED_SAMPLE_RATES_HZ[0]);
        for candidate in
            lossless_device_format_candidates(&header_source, ValidBitsAlignment::MostSignificant)
        {
            let label = format!(
                "{}B v{}",
                candidate.container_bytes, candidate.valid_bits_per_sample
            );
            header.push_str(&format!(" {label:>COLUMN_WIDTH$}"));
        }
        println!("{header}");
        for sample_rate_hz in PROBED_SAMPLE_RATES_HZ {
            let source = stereo_source(source_bits, sample_rate_hz);
            let candidates =
                lossless_device_format_candidates(&source, ValidBitsAlignment::MostSignificant);
            let mut row = format!("{sample_rate_hz:>10}");
            match output.probe_exclusive_candidates(&device.id, &candidates) {
                Ok(probes) => {
                    for probe in &probes {
                        row.push_str(&format!(" {:>COLUMN_WIDTH$}", describe_probe(probe)));
                    }
                }
                Err(error) => row.push_str(&format!(" error: {error}")),
            }
            println!("{row}");
        }
    }

    fn stereo_source(bits_per_sample: u8, sample_rate_hz: u32) -> StreamFormat {
        StreamFormat {
            sample_rate_hz,
            bits_per_sample,
            channel_count: PROBED_CHANNEL_COUNT,
            channel_mask: ChannelMask::default_for_flac_channel_count(PROBED_CHANNEL_COUNT)
                .unwrap_or(ChannelMask(0x3)),
        }
    }

    fn describe_probe(probe: &ExclusiveCandidateProbe) -> String {
        match (probe.extensible_outcome, probe.plain_wave_format_outcome) {
            (FormatProbeOutcome::Accepted, _) => "yes".to_owned(),
            (_, Some(FormatProbeOutcome::Accepted)) => "yes(WFX)".to_owned(),
            (outcome, _) => describe_outcome(outcome),
        }
    }

    fn describe_outcome(outcome: FormatProbeOutcome) -> String {
        match outcome {
            FormatProbeOutcome::Accepted => "yes".to_owned(),
            FormatProbeOutcome::Refused => "no".to_owned(),
            FormatProbeOutcome::Error { platform_code } => {
                format!("{:#010x}", platform_code as u32)
            }
        }
    }

    fn print_legend() {
        println!();
        println!(
            "legend: column \"3B v24\" = 3-byte container, 24 valid bits (WAVEFORMATEXTENSIBLE, PCM)."
        );
        println!("        yes = accepted; no = AUDCLNT_E_UNSUPPORTED_FORMAT;");
        println!(
            "        yes(WFX) = refused as WAVEFORMATEXTENSIBLE, accepted as plain WAVEFORMATEX;"
        );
        println!(
            "        0x8889000e = exclusive mode disabled in Windows; other hex = HRESULT error."
        );
    }
}
