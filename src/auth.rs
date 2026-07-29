//! Credentials the user can replace without restarting the process.
//!
//! `Runner` resolves its bearer once and then holds whatever credential source it was given, and a
//! `DeviceGrant` caches the credential it obtained in memory for the access token's whole hour. So
//! re-enrolling behind its back has no effect: the runner keeps presenting the token it already has.
//! [`SwappableCredentials`] is the seam that makes `/login` mean something - the source underneath
//! it can be exchanged, and the next dial picks up whatever is there.

use std::sync::{Arc, RwLock};

use zyris::enroll::{CredentialStore, Enroller, FileCredentialStore};
use zyris::runtime::{Credentials, CredentialsError, DeviceGrant};

/// A credential source with a replaceable inside.
pub struct SwappableCredentials {
    inner: RwLock<Arc<dyn Credentials>>,
}

impl SwappableCredentials {
    pub fn new(initial: Arc<dyn Credentials>) -> SwappableCredentials {
        SwappableCredentials {
            inner: RwLock::new(initial),
        }
    }

    /// Clone the Arc out rather than holding the guard: `bearer` awaits, and a `std` guard must not
    /// be alive across that.
    fn current(&self) -> Arc<dyn Credentials> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn swap(&self, next: Arc<dyn Credentials>) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

#[zyris::async_trait]
impl Credentials for SwappableCredentials {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        self.current().bearer().await
    }

    async fn refresh(&self) -> Result<bool, CredentialsError> {
        self.current().refresh().await
    }

    fn describe(&self) -> String {
        self.current().describe()
    }
}

/// Owns everything needed to enroll this node again from scratch.
pub struct Authenticator {
    creds: Arc<SwappableCredentials>,
    server_url: String,
    profile: String,
    node_name: String,
    platform: String,
    scopes: Vec<String>,
}

impl Authenticator {
    pub fn new(
        creds: Arc<SwappableCredentials>,
        server_url: String,
        profile: String,
        node_name: String,
        platform: String,
        scopes: Vec<String>,
    ) -> Authenticator {
        Authenticator {
            creds,
            server_url,
            profile,
            node_name,
            platform,
            scopes,
        }
    }

    fn store(&self) -> Result<FileCredentialStore, String> {
        FileCredentialStore::for_server(&self.server_url, &self.profile).map_err(|e| e.to_string())
    }

    /// Forget the stored credential. The process keeps running on the token it already holds until
    /// the connection drops, which is the honest thing: nothing has been revoked server-side.
    pub async fn logout(&self) -> Result<(), String> {
        self.store()?.clear().await.map_err(|e| e.to_string())
    }

    /// Enroll again, interactively.
    ///
    /// The stored credential is cleared first so `obtain` takes the enrollment path rather than
    /// quietly refreshing the one already on disk - a refresh reuses the old grant, and the whole
    /// point of a re-login is to get a new one, possibly with different scopes.
    ///
    /// Prints to the real terminal, so the caller must have left the alternate screen first.
    pub async fn relogin(&self) -> Result<String, String> {
        let store = self.store()?;
        store.clear().await.map_err(|e| e.to_string())?;

        let enroller = Enroller::with_file_store(
            &self.server_url,
            &self.profile,
            self.node_name.clone(),
            self.platform.clone(),
            self.scopes.clone(),
        )
        .map_err(|e| e.to_string())?;

        // A fresh `DeviceGrant` holds nothing, so its first `bearer` runs the full decision tree and
        // lands on enrollment. Swapping it in afterwards means later refreshes keep working from it.
        let grant = Arc::new(DeviceGrant::new(enroller));
        let bearer = grant.bearer().await.map_err(|e| e.to_string())?;
        self.creds.swap(grant);
        Ok(zyris::runtime::token_prefix(&bearer).to_string())
    }

    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}
