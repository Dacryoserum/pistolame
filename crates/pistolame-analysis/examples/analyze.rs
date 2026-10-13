//! Prints the authenticity verdict of a file: `cargo run --release -p pistolame-analysis --example analyze -- <file>`.
#![allow(clippy::print_stdout)]

fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: analyze <file>");
        return;
    };
    let started = std::time::Instant::now();
    match pistolame_analysis::analysis::analyze(std::path::Path::new(&path)) {
        Ok(result) => {
            println!("{:?}", result.transcode_assessment);
            println!("bit depth: {:?}", result.bit_depth_analysis);
            println!("sample rate: {:?}", result.sample_rate_analysis);
            println!("dr: {:?}", result.dynamic_range);
            println!("signal: {:?}", result.signal_analysis);
            println!("file: {:?}", result.file_info);
            let s = &result.spectral_analysis;
            println!(
                "cutoff {:?} steep {} edge {:?}",
                s.spectral_cutoff_hz, s.rolloff_steepness_db_per_khz, s.encoder_edge_hz
            );
            println!("analysed in {:?}", started.elapsed());
        }
        Err(error) => eprintln!("{error}"),
    }
}
