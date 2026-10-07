//! The Lucide icons the chrome draws, declared once and installed into the
//! egui context that owns them (every window gets its own context, so this
//! runs again on each reopen — see `App::attach`).

fastframe_icons::icons! {
    /// Every icon fastcription draws. All of them come from the shared
    /// Lucide set; fastcription has none of its own yet, so `directory`
    /// names a path with nothing in it.
    pub enum Icon {
        prefix: "fastcription-icon-",
        directory: "../assets/icons/",
        Play => lucide "play",
        Pause => lucide "pause",
        Stop => lucide "circle-x",
        Mic => lucide "mic",
        Settings => lucide "settings",
        Search => lucide "search",
        Clock => lucide "clock",
        StatusOk => lucide "circle-check",
        StatusWarn => lucide "circle-alert",
        Rename => lucide "pencil",
        Export => lucide "external-link",
        Reconnect => lucide "refresh-cw",
        Groups => lucide "users",
        Tag => lucide "pin",
        Monitor => lucide "monitor",
    }
}
