//! Every user-facing string goes through [`t`] rather than appearing as a
//! literal in a view, so swapping this for `fastframe-i18n` catalogs later
//! touches one file, not every call site.
//!
//! `fastframe-i18n` itself was not wired in: it compiles PO catalogs at build
//! time and expects the app to ship `.po` files per locale, which is real
//! localisation work with nothing to translate yet. This indirection is the
//! seam for that, not a replacement for it.
pub fn t(s: &'static str) -> &'static str {
    s
}
