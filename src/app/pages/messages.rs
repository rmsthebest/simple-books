//! What each `?done=` and `?error=` code says. Anything not listed shows nothing, so
//! a crafted link can't put words on the page.

use crate::server::library::upload::SYNC_HINT;

pub fn done(code: &str) -> Option<&'static str> {
    Some(match code {
        "uploaded" | "added" => SYNC_HINT,
        "deleted" => "Deleted. It leaves any Kobo holding it at its next sync.",
        "removed" => "Removed from your library. It leaves your Kobo at its next sync.",
        "saved" => "Saved. Your Kobo picks this up at its next sync.",
        "cover" => "Cover changed. Your Kobo picks this up at its next sync.",
        "shared" => "This book is now shared with everyone.",
        "private" => "This book is now just for you.",
        "resync" => "Your Kobo will get everything again at its next sync.",
        "device_removed" => "That Kobo can no longer sync here.",
        "disabled" => "That user can no longer sign in.",
        "enabled" => "That user can sign in again.",
        "quota" => "Storage limit changed.",
        "hardcover_off" => "Hardcover is disconnected. Nothing more is sent there.",
        "hardcover_link" => "Thanks. It'll use that book on Hardcover from now on.",
        _ => return None,
    })
}

pub fn error(code: &str) -> Option<&'static str> {
    Some(match code {
        "wrong" => "That username and password don't match.",
        "slow" => "Too many tries. Please wait a few minutes and try again.",
        "mismatch" => "Those two passwords aren't the same.",
        "short" => "Passwords need at least 10 characters.",
        "long" => "That password is too long.",
        "expired" => "This link has expired or was already used. Ask your admin for a new one.",
        "details" => {
            "Please check that: a book needs a title, a series number must be a number, and an ISBN must be a real one."
        }
        "already_shared" => {
            "Someone else already shares this same file, so it can't be shared twice."
        }
        "username" => "Usernames are 1 to 64 characters, with no slashes or spaces at the ends.",
        "taken" => "That username is taken.",
        "self" => "You can't disable your own account.",
        "quota" => "A storage limit is a whole number of MB.",
        "not_a_book" => "That file couldn't be read as an EPUB.",
        "no_file" => "Please choose a file first.",
        "bad_form" => "That form had something unexpected in it.",
        "too_large" => "That file is larger than this server accepts.",
        "storage" => "You've reached your storage limit.",
        "busy" => "You're already uploading two books. Try again when one finishes.",
        "disk" => "The server is nearly out of disk space.",
        "interrupted" => "The upload was interrupted. Please try again.",
        "not_image" => "That isn't a JPEG, PNG or WebP image.",
        "server" => "Something went wrong on the server. Please try again.",
        "hardcover_down" => "Hardcover couldn't be reached. Please try again in a moment.",
        "hardcover_link" => {
            "That isn't a link to a book on Hardcover. It looks like https://hardcover.app/books/…"
        }
        _ => return None,
    })
}
