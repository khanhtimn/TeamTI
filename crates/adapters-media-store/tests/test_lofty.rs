use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use std::path::Path;

#[test]
fn test_write_lofty_tmp() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join("tests/fixtures/dummy.wav");

    let temp_path = Path::new("/tmp/test_dummy.uuid.tmp.wav");
    std::fs::copy(&path, temp_path).expect("failed to copy fixture");

    let mut tagged = Probe::open(temp_path).expect("open").read().expect("read");
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(lofty::tag::Tag::new(lofty::tag::TagType::Id3v2));
    }
    let tag = tagged.primary_tag_mut().unwrap();
    tag.set_title(String::from("Secondhand Test"));

    if let Err(e) = tagged.save_to_path(temp_path, WriteOptions::default()) {
        panic!("Write error: {:?}", e);
    }
}
