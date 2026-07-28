//! Resolving the user's monospaced font.
//!
//! The control panel's layout depends on fixed-width columns, but which
//! monospaced face the user actually wants is theirs to decide — normally
//! through fontconfig. Slint cannot ask for that itself: it wraps whatever
//! `default-font-family` holds in a *named* family lookup, so the CSS generic
//! `"monospace"` resolves to nothing and falls back to a proportional face.
//! Resolving the generic to a concrete family name here is what closes that
//! gap.

/// The user's preferred monospaced family, or `None` when the system offers
/// no monospaced font at all.
///
/// Queries the same font database Slint renders with, so the name returned is
/// one Slint is guaranteed to be able to look up.
pub fn preferred_monospace_family() -> Option<String> {
    let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());

    // fontconfig returns a preference-ordered chain; its head is the answer.
    // UiMonospace first because a desktop that expresses a separate interface
    // preference means it, and it is empty rather than wrong when unset.
    for generic in [
        fontique::GenericFamily::UiMonospace,
        fontique::GenericFamily::Monospace,
    ] {
        let first = collection.generic_families(generic).next();
        if let Some(id) = first
            && let Some(name) = collection.family_name(id)
        {
            return Some(name.to_owned());
        }
    }

    None
}
