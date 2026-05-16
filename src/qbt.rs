use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub struct Client {
    url: String,
    username: String,
    password: String,
    agent: ureq::Agent,
    cookie: Mutex<Option<String>>,
}

impl Client {
    #[must_use]
    pub fn new(url: &str, username: &str, password: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            username: username.to_owned(),
            password: password.to_owned(),
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                .build()
                .into(),
            cookie: Mutex::new(None),
        }
    }

    /// # Errors
    /// Returns an error if login fails (bad credentials, network error) or if the
    /// `setPreferences` request fails.
    ///
    /// # Panics
    /// Panics if the internal session-cookie mutex is poisoned, which can only
    /// happen if a previous thread panicked while holding it.
    pub fn set_listen_port(&self, port: u16) -> Result<()> {
        let prefs = format!(r#"{{"listen_port":{port}}}"#);

        // Try the cached session first; re-login only on auth failure.
        let cached = self.cookie.lock().expect("cookie mutex poisoned").clone();
        if let Some(ref c) = cached {
            match self.set_preferences(c, &prefs) {
                Ok(()) => return Ok(()),
                Err(e) if is_auth_error(&e) => {
                    *self.cookie.lock().expect("cookie mutex poisoned") = None;
                }
                Err(e) => return Err(e),
            }
        }

        let cookie = self.login()?;
        self.set_preferences(&cookie, &prefs)?;
        *self.cookie.lock().expect("cookie mutex poisoned") = Some(cookie);
        Ok(())
    }

    fn login(&self) -> Result<String> {
        let mut resp = self
            .agent
            .post(&format!("{}/api/v2/auth/login", self.url))
            .send_form([("username", &self.username), ("password", &self.password)])
            .context("qBittorrent login request")?;

        let cookie = resp
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or("").trim().to_owned());

        let body = resp
            .body_mut()
            .read_to_string()
            .context("read login response")?;

        if body.trim() == "Fails." {
            bail!("qBittorrent rejected the login credentials — check --qbt-user and --qbt-pass");
        }

        cookie.context("qBittorrent login succeeded but returned no session cookie (unexpected server response)")
    }

    fn set_preferences(&self, cookie: &str, prefs_json: &str) -> Result<()> {
        self.agent
            .post(&format!("{}/api/v2/app/setPreferences", self.url))
            .header("Cookie", cookie)
            .send_form([("json", prefs_json)])
            .context("setPreferences request")?;
        Ok(())
    }
}

fn is_auth_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        if let Some(ureq::Error::StatusCode(code)) = cause.downcast_ref::<ureq::Error>() {
            *code == 403
        } else {
            false
        }
    })
}
