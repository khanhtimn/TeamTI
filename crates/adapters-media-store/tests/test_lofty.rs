use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use std::path::Path;

#[test]
fn test_write_lofty_tmp() {
    let path = Path::new(
        "/Users/khanhtimn/Documents/project/teamti/data/Don Toliver/OCTANE/4 - Secondhand.flac",
    );
    let temp_path = Path::new("/tmp/test_secondhand.uuid.tmp.flac");
    std::fs::copy(path, temp_path).unwrap();

    let mut tagged = Probe::open(temp_path).expect("open").read().expect("read");
    let tag = tagged.primary_tag_mut().unwrap();
    tag.set_title(String::from("Secondhand Test"));

    if let Err(e) = tagged.save_to_path(temp_path, WriteOptions::default()) {
        panic!("Write error: {:?}", e);
    }
}
