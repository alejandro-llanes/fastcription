//! Every user-facing string goes through [`t`] or [`tf`] rather than appearing
//! as a literal in a view, so swapping this for `fastframe-i18n` catalogs later
//! touches one file, not every call site.
//!
//! `fastframe-i18n` itself was not wired in: it compiles PO catalogs at build
//! time and expects the app to ship `.po` files per locale, which is real
//! localisation work with nothing to translate yet. This indirection is the
//! seam for that, not a replacement for it.
pub fn t(s: &'static str) -> &'static str {
    s
}

/// As [`t`], for a message with something of the machine's in it.
///
/// A formatted message used to bypass this file entirely — `format!("Could not
/// create the group: {err}")` has no `&'static str` for a catalog to be keyed
/// on, so every message that named a file, a device or an error was quietly
/// untranslatable. Keeping the format string as the key and substituting
/// afterwards makes each one a single unit, which is also the only form a
/// translator can work with: word order moves between languages, and a message
/// glued together from three fragments cannot be reordered.
///
/// `{}` is filled from `args` in order. A placeholder with no argument is left
/// as it is rather than dropped, so a call site that lost an argument shows up
/// instead of silently losing what it was going to say.
pub fn tf(template: &'static str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut args = args.iter();
    let mut rest = t(template);
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        match args.next() {
            Some(arg) => out.push_str(arg),
            None => out.push_str("{}"),
        }
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::tf;

    #[test]
    fn placeholders_are_filled_in_order() {
        assert_eq!(
            tf("Could not {} {}: no such file", &["read", "the library"]),
            "Could not read the library: no such file"
        );
        assert_eq!(tf("nothing to fill", &[]), "nothing to fill");
        assert_eq!(tf("{}", &["whole thing"]), "whole thing");
    }

    /// Neither of these is worth a panic in a repaint, and both are visible
    /// enough in the result to be noticed and fixed.
    #[test]
    fn a_mismatched_call_site_degrades_rather_than_panicking() {
        assert_eq!(tf("{} and {}", &["one"]), "one and {}");
        assert_eq!(tf("just {}", &["one", "two"]), "just one");
    }

    /// Braces that are not a placeholder are text: a transcript or a path may
    /// contain them, and `format!`'s escaping rules do not apply here.
    #[test]
    fn other_braces_are_left_alone() {
        assert_eq!(tf("a {brace} and {}", &["this"]), "a {brace} and this");
        assert_eq!(tf("{{}}", &["x"]), "{x}");
    }
}
