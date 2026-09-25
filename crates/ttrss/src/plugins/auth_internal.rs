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
//! - `change_password` — verify the old password, set a fresh salt +
//!   SSHA-512 hash. Dispatched via `invoke_method` (the `IAuthModule2`
//!   method, not a hook).
//!
//! The core auth logic lives in [`crate::auth`] (the `UserHelper` port);
//! this plugin wraps it in the `Plugin::authenticate` contract.

use crate::auth;
use async_trait::async_trait;
use crate::pluginhost::host::{HostCtx, PluginHost};
use crate::i18n::{__};
use crate::pluginhost::plugin::{About, MethodOutput, Plugin, PluginError};
use crate::pluginhost::HOOK_AUTH_USER;
use serde_json::Value;

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
        host: &mut HostCtx<'_>,
        login: &str,
        password: &str,
        _service: &str,
    ) -> Option<i32> {
        let outcome = auth::authenticate(host.pool, login, password, None).await;
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

    /// `IAuthModule2::change_password` — dispatched via the `PluginHandler`
    /// backend (`op=pluginhandler&plugin=auth_internal&method=changepassword`).
    fn has_public_method(&self, method: &str) -> bool {
        method.eq_ignore_ascii_case("changepassword")
    }

    async fn invoke_method(
        &self,
        host: &mut HostCtx<'_>,
        method: &str,
        params: &Value,
    ) -> Result<MethodOutput, PluginError> {
        match method.to_ascii_lowercase().as_str() {
            "changepassword" => {
                let uid = host.owner_uid.unwrap_or(0);
                let old_pw = params
                    .get("old_password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_pw = params
                    .get("new_password")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match auth::change_password(host.pool, uid, old_pw, new_pw).await {
                    Some(_) => Ok(MethodOutput::Raw(
                        __("Password has been changed."),
                    )),
                    None => Ok(MethodOutput::Raw(
                        format!("ERROR: {}", __("Old password is incorrect.")),
                    )),
                }
            }
            _ => Err(PluginError::UnknownMethod(method.to_string())),
        }
    }
}
