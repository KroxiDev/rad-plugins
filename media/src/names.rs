//! Which application a bus name belongs to, and which instance of it is used.

/// Every MPRIS player owns a bus name that starts with this.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// The application behind an MPRIS bus name, or `None` if it is not an MPRIS name.
///
/// This is the part after [`MPRIS_PREFIX`], without a trailing `.instance<digits>`: browsers
/// suffix their name with the process that owns it (`...brave.instance4242`), so each of them
/// is one application whatever instance is running.
pub fn app_identity(bus_name: &str) -> Option<&str> {
    let name = bus_name.strip_prefix(MPRIS_PREFIX)?;
    let identity = match name.rsplit_once(".instance") {
        Some((app, digits))
            if !app.is_empty()
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            app
        }
        _ => name,
    };
    (!identity.is_empty()).then_some(identity)
}

/// Among `bus_names`, the instance of the application `identity` that the plugin uses.
///
/// When several instances run, the one whose bus name sorts first as text is used. The bus lists
/// names in no meaningful order, and this one does not depend on it: the same instance is picked
/// by every action and every visual, now and later.
pub fn first_instance<'a>(
    bus_names: impl IntoIterator<Item = &'a str>,
    identity: &str,
) -> Option<&'a str> {
    bus_names
        .into_iter()
        .filter(|name| app_identity(name) == Some(identity))
        .min()
}
