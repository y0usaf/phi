//! UI hooks for login flows. Frontends print/show; headless callers can rely
//! on the callback server alone.

use std::time::Duration;

/// UI hooks for login flows. Frontends print/show; headless callers can rely
/// on the callback server alone.
pub trait LoginCallbacks: Send + Sync {
    fn on_auth_url(&self, _url: &str, _instructions: &str) {}
    fn on_device_code(&self, _user_code: &str, _verification_uri: &str, _expires_in: Duration) {}
    fn on_progress(&self, _message: &str) {}
    /// Resolves when the user pastes a code/redirect URL manually. The
    /// default never resolves, leaving only the callback-server path.
    fn manual_code(&self) -> std::pin::Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Print-to-stdout callbacks for headless `--login`.
pub struct PrintCallbacks;

impl LoginCallbacks for PrintCallbacks {
    fn on_auth_url(&self, url: &str, instructions: &str) {
        println!("{instructions}\n\n  {url}\n");
    }

    fn on_device_code(&self, user_code: &str, verification_uri: &str, expires_in: Duration) {
        println!(
            "Visit {verification_uri} and enter code: {user_code} (expires in {} minutes)",
            expires_in.as_secs() / 60
        );
    }

    fn on_progress(&self, message: &str) {
        println!("{message}");
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoginMethod {
    #[default]
    Browser,
    DeviceCode,
}
