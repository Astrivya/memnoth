//! Sign-in from inside the session screen.
//!
//! The device flow blocks for as long as it takes someone to approve a code in
//! a browser — up to the code's `expires_in`, a minute or more. That cannot run
//! on the thread that draws frames, so `/login` starts this worker and the run
//! loop reports what it says. Nothing here prints: the transcript is the only
//! place a message may appear, and only the run loop may write to it.
//!
//! This worker never opens a browser. In the session screen the link is already
//! on a line the person can select, and a browser jumping to the front mid-edit
//! is the kind of surprise `/login` should not be able to cause.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::auth::{await_device_login, begin_device_login, enrich_login, store_login};
use crate::context::AppContext;

/// The one-time code and the link, held on the screen while the wait runs.
///
/// The transcript also gets them, as history. This copy is the one that cannot
/// be lost: a cleared transcript, or a long one scrolled past, would otherwise
/// leave a wait with a code nobody can read and nothing to open.
#[derive(Debug, Clone)]
pub struct LoginPrompt {
    pub user_code: String,
    pub url: String,
}

/// What the worker tells the screen.
#[derive(Debug, Clone)]
pub enum LoginEvent {
    /// Show this to the person: the code to type and the URL that is open.
    Code { user_code: String, url: String },
    /// Signed in, and the credential is already stored: `email` is the identity
    /// the flow reported, not something to look up first.
    Done { email: String, org_name: String },
    /// The organisation arrived after the credential did.
    Enriched { org_name: String, role: String },
    /// The person stopped the wait before anyone approved: nothing was stored.
    Cancelled,
    /// Nothing was stored; the message is ready for the transcript.
    Failed(String),
}

/// Start the device flow on a background thread. Returns at once.
///
/// The context is accepted so a caller has one shape for the whole session
/// lifecycle, but the flow reads the credential store on its own and needs no
/// path from it. A send that fails only ever means the session ended first, so
/// it is not an error: the thread simply stops.
///
/// `cancel` is the screen's handle on the wait: setting it stops the next poll
/// and reports [`LoginEvent::Cancelled`] instead of storing anything. It is an
/// atomic rather than a channel because the answer is needed inside a sleep.
pub fn start(
    _ctx: AppContext,
    client_name: String,
    tx: mpsc::Sender<LoginEvent>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || run(client_name, tx, cancel));
}

fn run(client_name: String, tx: mpsc::Sender<LoginEvent>, cancel: Arc<AtomicBool>) {
    let (client, auth) = match begin_device_login(&client_name) {
        Ok(pair) => pair,
        Err(error) => {
            let _ = tx.send(LoginEvent::Failed(format!("{error:#}")));
            return;
        }
    };

    // There is nothing to open here, so the code and the URL go straight to the
    // screen: the link is the only way in, and it is on a line of its own.
    if tx
        .send(LoginEvent::Code {
            user_code: auth.user_code.clone(),
            url: auth.verification_uri_complete.clone(),
        })
        .is_err()
    {
        return;
    }

    // The screen cannot be cancelled from here — a keystroke in the session
    // screen belongs to the prompt — so the wait is sliced and the flag is read
    // between slices: `/login` again ends the attempt now, not after the next
    // poll interval.
    let mut quiet = |_: crate::auth::LoginPhase| {};
    let mut sleep = |wait: std::time::Duration| {
        let step = std::time::Duration::from_millis(100);
        let mut left = wait;
        while left > std::time::Duration::ZERO {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let slice = left.min(step);
            std::thread::sleep(slice);
            left -= slice;
        }
        !cancel.load(Ordering::Relaxed)
    };
    let token = match await_device_login(&client, &auth, &mut quiet, &mut sleep) {
        Ok(Some(token)) => token,
        Ok(None) => {
            let _ = tx.send(LoginEvent::Cancelled);
            return;
        }
        Err(error) => {
            let _ = tx.send(LoginEvent::Failed(format!("{error:#}")));
            return;
        }
    };

    // The device flow does not report the tier it granted; `unknown` is the
    // key the server minted. Storing is local, so the screen can say who this
    // is on the next frame instead of after the org read.
    let stored = match store_login(&token, None) {
        Ok(stored) => stored,
        Err(error) => {
            let _ = tx.send(LoginEvent::Failed(format!("{error:#}")));
            return;
        }
    };
    if tx
        .send(LoginEvent::Done {
            email: stored.email.clone(),
            org_name: stored.org_name.clone(),
        })
        .is_err()
    {
        return;
    }

    // Enrichment is a second line, never a second sign-in: a network that cannot
    // answer it leaves the credential exactly as it was.
    if let Ok(enriched) = enrich_login(&token, None)
        && !enriched.org_name.is_empty()
    {
        let _ = tx.send(LoginEvent::Enriched {
            org_name: enriched.org_name,
            role: String::new(),
        });
    }
}

/// One phrasing for the signed-in state, so the brief, the transcript and the
/// startup screen all say the same thing.
pub fn status_line(account: Option<&str>) -> String {
    match account {
        Some(email) => format!("signed in as {email}"),
        None => "not signed in · /login".to_string(),
    }
}
