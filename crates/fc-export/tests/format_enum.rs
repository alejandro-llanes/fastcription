use fc_export::ExportFormat;

#[test]
fn round_trips_every_format() {
    for format in [
        ExportFormat::Text,
        ExportFormat::Markdown,
        ExportFormat::Json,
        ExportFormat::Srt,
        ExportFormat::Vtt,
    ] {
        let parsed = ExportFormat::parse(format.as_str());
        assert_eq!(parsed, Some(format));
    }
}

#[test]
fn accepts_file_extension_shorthand() {
    assert_eq!(ExportFormat::parse("txt"), Some(ExportFormat::Text));
    assert_eq!(ExportFormat::parse("md"), Some(ExportFormat::Markdown));
}

#[test]
fn is_case_insensitive() {
    assert_eq!(ExportFormat::parse("JSON"), Some(ExportFormat::Json));
    assert_eq!(ExportFormat::parse("Srt"), Some(ExportFormat::Srt));
}

#[test]
fn rejects_unknown_format() {
    assert_eq!(ExportFormat::parse("pdf"), None);
}

#[test]
fn extensions_match_the_common_convention() {
    assert_eq!(ExportFormat::Text.extension(), "txt");
    assert_eq!(ExportFormat::Markdown.extension(), "md");
    assert_eq!(ExportFormat::Json.extension(), "json");
    assert_eq!(ExportFormat::Srt.extension(), "srt");
    assert_eq!(ExportFormat::Vtt.extension(), "vtt");
}
