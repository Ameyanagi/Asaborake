//! Logo learning, location and detection.
//!
//! The pipeline is three stages, each with its own module:
//!
//! 1. [`locate`] finds *where* the logo is, from the fact that a logo is an
//!    edge that never moves.
//! 2. [`scan`] learns *what* it is, by regressing observed pixels against the
//!    background implied by frames whose logo surroundings are flat.
//! 3. [`detect`] scores every frame against it, and [`track`] turns those
//!    scores into the presence intervals CM detection consumes.

pub mod detect;
pub mod locate;
pub mod model;
pub mod scan;
pub mod track;

pub use detect::LogoDetector;
pub use locate::LogoLocator;
pub use model::{LogoData, Rect};
pub use scan::{DEFAULT_FLATNESS_THRESHOLD, LogoScanner};
pub use track::{LogoInterval, LogoTrack, TrackOptions};
