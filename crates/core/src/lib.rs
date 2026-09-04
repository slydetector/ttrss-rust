//! tt-rss domain core (no HTTP).
//!
//! Byte-for-byte ports of the PHP primitives the server depends on: PHP
//! serialization, DB-backed encrypted sessions, user password/OTP/app-password
//! verification.

pub mod config;
pub mod crypt;
pub mod db;
pub mod phpser;
pub mod session;
pub mod user;
