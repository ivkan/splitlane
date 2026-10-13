//! The ids a pane's program knows its pane by, when they are not the pane's.
//!
//! A shell is told which pane it runs in (`SPLITLANE_SURFACE_ID`), and its
//! hooks and its calls to the app say that id back. Ordinarily it is the
//! pane's own id, and this table is empty.
//!
//! A session in the session host outlives the window that started it. The
//! next window shows it in a pane with another id, while the shell and
//! everything under it go on saying the one they were told. So this table
//! holds "the id a program says" to "the pane it means now", and whatever
//! takes a pane's id from outside the process looks here first.
//!
//! # Why a shell in the host is not told its pane's own id
//!
//! A pane's own id is a small number that every window hands out again. If
//! a shell were told that number, then in the next window the number would
//! quite likely also be the own id of some *other* pane, and a caller
//! saying it could mean either: the old shell means its pane, a script that
//! read the number from a listing means the other one. Nothing in the
//! request tells them apart. This was not hypothetical: in the first live
//! run, the second pane the new window made got exactly the id the
//! reattached shell had been told.
//!
//! So a shell that is started in the host is told an id made up for it,
//! from a range no window hands out. The two kinds of id can then never be
//! taken for one another, in this window or any later one.

// Only a window whose panes can run in the session host writes the table.
// Everybody reads it.
#![cfg_attr(not(host_panes), allow(dead_code))]

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// Made-up ids are this bit with 51 random bits below it: at or above 2^51
/// and below 2^52.
///
/// Above anything a window hands out by itself - those are a slot number
/// with a reuse count in the upper half, and one slot would have to be
/// reused half a million times to reach this far. Below 2^53, because
/// agent plugins written in JavaScript read the id from the environment
/// and send it back as a number, and a number above that is not exact
/// there.
const MADE_UP_BASE: u64 = 1 << 51;

static TABLE: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);

fn table() -> MutexGuard<'static, Option<HashMap<u64, u64>>> {
    // Nothing guarded here is left half-changed by a panic.
    TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `id` is one that was made up for a shell, in this window or
/// another, and so never a pane's own.
#[cfg(test)]
fn is_made_up(id: u64) -> bool {
    (MADE_UP_BASE..MADE_UP_BASE << 1).contains(&id)
}

/// An id to tell a shell that is about to be started in the session host
/// for the pane `pane`, recorded as meaning that pane.
pub(crate) fn tell(pane: u64) -> u64 {
    let mut table = table();
    let table = table.get_or_insert_with(HashMap::new);
    loop {
        let random = uuid::Uuid::new_v4().as_u128() as u64;
        let made_up = MADE_UP_BASE | (random & (MADE_UP_BASE - 1));
        if let std::collections::hash_map::Entry::Vacant(free) = table.entry(made_up) {
            free.insert(pane);
            return made_up;
        }
    }
}

/// A program that says `said` means the pane `pane` from now on: the
/// program of a session that was started under another window and is shown
/// in `pane` now.
pub(crate) fn remember(said: u64, pane: u64) {
    table().get_or_insert_with(HashMap::new).insert(said, pane);
}

/// The pane `pane` is gone; nothing means it any more.
pub(crate) fn forget(pane: u64) {
    if let Some(table) = table().as_mut() {
        table.retain(|_, meant| *meant != pane);
    }
}

/// The pane a program means by `said`: what is recorded for it, and `said`
/// itself when nothing is, which is every id while no pane runs in the
/// session host.
pub(crate) fn pane_meant(said: u64) -> u64 {
    table()
        .as_ref()
        .and_then(|table| table.get(&said).copied())
        .unwrap_or(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ids of this test's own: the table is the process's, and other tests
    // run beside this one.
    #[test]
    fn what_a_program_says_means_the_pane_that_shows_it_now() {
        // Nothing known: an id is its own pane.
        assert_eq!(pane_meant(9_000_001), 9_000_001);

        // A shell started for the pane 9_000_002 is told an id that no
        // window hands out, and no other shell is told the same.
        let told = tell(9_000_002);
        assert!(is_made_up(told));
        assert!(!is_made_up(9_000_002));
        assert!(told < 1 << 53);
        assert_ne!(tell(9_000_003), told);
        assert_eq!(pane_meant(told), 9_000_002);
        // The pane's own id is still the pane's own.
        assert_eq!(pane_meant(9_000_002), 9_000_002);

        // The window goes, and the next one shows the session in the pane
        // 9_000_010 - while some other pane of it happens to have the id
        // the first pane had. Neither is taken for the other.
        forget(9_000_002);
        assert_eq!(pane_meant(told), told);
        remember(told, 9_000_010);
        assert_eq!(pane_meant(told), 9_000_010);
        assert_eq!(pane_meant(9_000_002), 9_000_002);

        forget(9_000_010);
        forget(9_000_003);
        assert_eq!(pane_meant(told), told);
    }
}
