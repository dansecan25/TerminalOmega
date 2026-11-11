//! Saved SSH passwords, kept in the system keychain: Keychain on macOS, and
//! the Secret Service (GNOME Keyring, KWallet) on Linux. The keychain encrypts
//! them, and the app never shows them again once saved.

use keyring::{Entry, Error};

const SERVICE: &str = "terminal-omega";

fn entry(session_id: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, session_id).map_err(|e| describe(&e))
}

fn describe(error: &Error) -> String {
    match error {
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) | Error::NoDefaultStore => {
            format!("the system keychain isn't available ({error})")
        }
        _ => error.to_string(),
    }
}

pub fn save(session_id: &str, password: &str) -> Result<(), String> {
    entry(session_id)?
        .set_password(password)
        .map_err(|e| describe(&e))
}

pub fn load(session_id: &str) -> Result<String, String> {
    match entry(session_id)?.get_password() {
        Ok(password) => Ok(password),
        Err(Error::NoEntry) => Err("the saved password is missing from the keychain".into()),
        Err(e) => Err(describe(&e)),
    }
}

/// Deleting a password that isn't there counts as success.
pub fn delete(session_id: &str) -> Result<(), String> {
    match entry(session_id)?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(describe(&e)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "uses the real system keychain"]
    fn round_trip() {
        let id = crate::config::new_session_id();
        super::save(&id, "s3cret").unwrap();
        assert_eq!(super::load(&id).unwrap(), "s3cret");
        super::delete(&id).unwrap();
        assert!(super::load(&id).is_err());
        super::delete(&id).unwrap();
    }
}
