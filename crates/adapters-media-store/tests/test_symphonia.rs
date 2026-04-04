use std::fs::File;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;
use symphonia::default::get_probe;

#[test]
fn test_symphonia_meta() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join("tests/fixtures/dummy.wav");

    let src = File::open(&path).expect("open");
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("wav");
    let probed = get_probe()
        .format(&hint, mss, &Default::default(), &Default::default())
        .unwrap();
    let track = probed.format.default_track().unwrap();
    let params = &track.codec_params;

    if let Some(ch) = params.channels {
        println!("channels count: {}", ch.count());
    }
}
