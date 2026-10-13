//! M7.20 — the **`auth_internal`** plugin (authenticate against the
//! internal tt-rss database).
//!
//! PHP reference: `tt-rss/plugins/auth_internal/init.php` (300 LOC).
//!
//! System plugin (`about().system = true`) implementing `IAuthModule2`:
//! - `authenticate` — the full login flow: OTP check → throttle → password
//!   hash verify (with algo upgrade). The OTP challenge and failure
//!   message are communicated via `HostCtx` side-channels
//!   (`otp_challenge`, `login_error`).
//! The core auth logic lives in [`crate::auth`] (the `UserHelper` port);
//! this plugin wraps it in the `Plugin::authenticate` contract.
//!
//! (The PHP's `Auth_Internal::change_password` is not exposed as a public
//! plugin method — the port has no plugin password-change route either; see
//! the NOTE in the impl below.)

use crate::auth;
use crate::pluginhost::HOOK_AUTH_USER;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::pluginhost::plugin::{About, Plugin};
use async_trait::async_trait;

/// PHP's `class Auth_Internal extends Auth_Base implements IAuthModule2`.
#[derive(Clone, Copy)]
pub struct AuthInternal;

#[async_trait]
impl Plugin for AuthInternal {
    fn name(&self) -> &'static str {
        "auth_internal"
    }
    fn class_name(&self) -> &'static str {
        "Auth_Internal"
    }
    fn about(&self) -> About {
        About {
            version: None,
            description: "Authenticates against internal tt-rss database".into(),
            author: "fox".into(),
            system: true,
            more_info: String::new(),
        }
    }
    fn api_version(&self) -> i32 {
        2
    }

    fn init(&self, host: &mut PluginHost) {
        host.add_hook_default(HOOK_AUTH_USER);
    }

    /// `Auth_Internal::authenticate` — the full login flow.
    ///
    /// Returns `Some(uid)` on success. On failure, sets `host.login_error`
    /// (the message to display) and/or `host.otp_challenge` (render the
    /// OTP challenge page) and returns `None`.
    async fn authenticate(
        &self,
        host: &mut HostCtx,
        login: &str,
        password: &str,
        _service: &str,
    ) -> Option<i32> {
        let outcome = auth::authenticate(host.pool(), login, password, None).await;
        match outcome {
            auth::AuthOutcome::Success { id, .. } => Some(id),
            auth::AuthOutcome::OtpChallenge => {
                host.otp_challenge = true;
                None
            }
            auth::AuthOutcome::Failure { message } => {
                host.login_error = Some(message);
                None
            }
        }
    }

    // NOTE: the PHP has no plugin password-change route — `Plugin::
    // is_public_method` defaults to `false` and `Auth_Internal` has no
    // `changepassword` method, so both plugin routes reject it. The port
    // matches: passwords change only via `Pref_Prefs` (`op=Pref_Prefs&
    // method=changepassword` → `settings::changepassword`), which refreshes
    // the session `pwd_hash`.
}
