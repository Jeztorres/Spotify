//! Lógica pura de SpotifyMac, portada de Swift. No depende de ninguna UI ni del sistema operativo.

pub mod color;
pub mod catalog;
pub mod engine_protocol;
pub mod models;
pub mod playlist_ordering;
pub mod queue;
pub mod search_ranking;
pub mod soloist_protocol;
pub mod text;

pub use models::*;
