mod common;

use fc_export::{export, write, ExportFormat, ExportOptions};

#[test]
fn write_matches_export() {
    let conversation = common::conversation();
    let segments = common::segments();
    let options = ExportOptions::default();

    let expected = export(&conversation, &segments, ExportFormat::Markdown, &options);

    let mut buf: Vec<u8> = Vec::new();
    write(&conversation, &segments, ExportFormat::Markdown, &options, &mut buf).expect("writes");

    assert_eq!(String::from_utf8(buf).unwrap(), expected);
}
