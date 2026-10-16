//! List filter persistence for the pull-requests route (`t3.pullRequests.preferences`).

use std::io;

use crate::hooks::use_local_storage::SharedLocalStorage;
use crate::pull_request::pull_request_list_preferences::PreferenceStorage;

pub struct LocalPreferenceStorage(pub SharedLocalStorage);

impl PreferenceStorage for LocalPreferenceStorage {
    fn get_item(&self, key: &str) -> io::Result<Option<String>> {
        self.0
            .get_item(key)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))
    }

    fn set_item(&self, key: &str, value: &str) -> io::Result<()> {
        self.0
            .set_item(key, value)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))
    }
}
