//! Minimal SDK for RAD Action Deck plugins: framing, protocol types, a dispatch loop with
//! deferred replies and visual heartbeats, and the `radp-pack` packer.
//!
//! ```no_run
//! use rad_plugin_sdk::*;
//!
//! struct Hello;
//! impl Plugin for Hello {
//!     fn invoke(&self, _call: Invoke, reply: Reply<Outcome>) {
//!         // Slow work goes on another thread; the reply can be sent from anywhere.
//!         std::thread::spawn(move || reply.send(Outcome::Success));
//!     }
//!     fn options(&self, _query: OptionsQuery, reply: Reply<ChoiceList>) {
//!         reply.send(ChoiceList::Unavailable);
//!     }
//!     fn subscribe(&self, subscription: Subscription) {
//!         subscription.update(VisualState::Current {
//!             content: VisualContent { text: Some("Hello".into()), ..Default::default() },
//!         });
//!     }
//!     fn unsubscribe(&self, _subscription: SubscriptionId) {}
//! }
//!
//! let version = Version { major: 1, minor: 0, patch: 0 };
//! let _ = run_stdio(Config::new("org.example.hello", version, 2..=2), Hello);
//! ```
mod error;
pub mod frame;
mod licenses;
pub mod pack;
mod protocol;
mod reply;
mod runtime;
mod subscription;
mod writer;

pub use error::Error;
pub use licenses::licenses_exit;
pub use protocol::*;
pub use reply::{Answer, Reply};
pub use runtime::*;
pub use subscription::{Subscription, SubscriptionId};

mod credentials;
pub use credentials::{CredentialValue, Credentials, MAX_CREDENTIAL_REVISIONS};

mod json;
