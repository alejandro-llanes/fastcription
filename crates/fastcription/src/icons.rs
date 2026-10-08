//! The Lucide icons the chrome draws, declared once and installed into the
//! egui context that owns them (every window gets its own context, so this
//! runs again on each reopen — see `App::attach`).

fastframe_icons::icons! {
    /// Every icon fastcription draws, and nothing else. All of them come from
    /// the shared Lucide set; fastcription has none of its own yet, so
    /// `directory` names a path with nothing in it.
    ///
    /// The list used to carry six more — a pencil, a magnifier, a clock, a
    /// settings cog — declared against a future that drew them nowhere. Each
    /// one is a few hundred bytes of SVG in the binary and, worse, a reader of
    /// this file inferring that the interface has a search button. An icon
    /// earns its place by saying something a word next to it does not.
    pub enum Icon {
        prefix: "fastcription-icon-",
        directory: "../assets/icons/",
        /// Start, and Resume.
        Play => lucide "play",
        Pause => lucide "pause",
        Stop => lucide "circle-x",
        /// The second-track toggle: this one is the user's own voice, as
        /// against the `Monitor` that labels the selected source.
        Mic => lucide "mic",
        /// The level meter, which reads the chosen source and not the
        /// microphone.
        Monitor => lucide "monitor",
        /// Re-enumerate the sources, next to the picker.
        Reconnect => lucide "refresh-cw",
        /// The readiness checklist, the notices and the service pill.
        StatusOk => lucide "circle-check",
        StatusWarn => lucide "circle-alert",
        Export => lucide "external-link",
        /// Leave compact mode. The caption strip is too narrow to spend on the
        /// word "Restore", and the two arrows say "make this big again" in a
        /// shape anyone has already seen on a video player.
        Restore => lucide "maximize-2",
        /// Enter compact mode: the opposite of `Restore`, and drawn as its
        /// mirror so the pair reads as one toggle.
        Compact => lucide "minimize-2",
    }
}
